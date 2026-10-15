#!/usr/bin/env bash
# Orchestrate the memory benchmark for one or more stacks.
#
#   bench/measure.sh [--fast] [--runs N] [--no-build] [--stack NAME]...
#
# Stacks: tpmcp datalayer block cursor marimo   (default: all five)
#
# Per stack and run: a pristine copy of bench/fixture lands in bench/work/
# (the fixtures themselves are never modified), the container starts, the
# sampler watches the cgroup memory and the process tree, and the driver
# runs the fixed operation profile over MCP. Results are JSON under
# bench/results/<stack>/run<N>/ - render.py turns them into COMPARISON.md.
#
# --fast shortens the quiesce windows (pilot runs on a busy machine);
# published numbers use the normal mode (60 s idle / 30 s post-op quiesce,
# 5 samples per window, median).
set -euo pipefail
cd "$(dirname "$0")"

ALL_STACKS="tpmcp datalayer block cursor marimo analyzer runpython yzfly"
fast=false
runs=1
do_build=true
stacks=""

while [ $# -gt 0 ]; do
    case "$1" in
        --fast) fast=true ;;
        --runs) shift; runs="$1" ;;
        --no-build) do_build=false ;;
        --stack) shift; stacks="$stacks $1" ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done
if [ -z "$stacks" ]; then
    stacks="$ALL_STACKS"
fi

command -v docker >/dev/null || { echo "docker not found" >&2; exit 1; }
command -v python3 >/dev/null || { echo "python3 not found" >&2; exit 1; }

build_stack() {
    case "$1" in
        tpmcp)
            [ -f tpmcp/tpmcp ] || { echo "bench/tpmcp/tpmcp missing (download the musl release asset)" >&2; exit 1; }
            docker build -q -t bench-tpmcp:pin tpmcp >/dev/null
            ;;
        datalayer | block)
            docker build -q -t bench-jlab-base:pin -f jlab-base.Dockerfile . >/dev/null
            docker build -q -t "bench-$1:pin" "$1" >/dev/null
            ;;
        cursor | marimo | runpython | yzfly)
            docker build -q -t "bench-$1:pin" "$1" >/dev/null
            ;;
        analyzer)
            # Official published image; the digest in meta.json is the real
            # pin (the upstream tag floats).
            docker pull -q ghcr.io/anselmoo/mcp-server-analyzer:latest >/dev/null
            docker tag ghcr.io/anselmoo/mcp-server-analyzer:latest bench-analyzer:pin
            ;;
        *)
            echo "unknown stack: $1" >&2; exit 2
            ;;
    esac
    size=$(docker image inspect --format '{{.Size}}' "bench-$1:pin")
    echo "  image bench-$1:pin = $((size / 1024 / 1024)) MB"
}

is_http() {
    case "$1" in
        marimo) return 0 ;;
        *) return 1 ;;
    esac
}

run_one() {
    stack="$1"
    run_index="$2"
    rd="results/$stack/run$run_index"
    wd="work/$stack"
    rm -rf "$rd" "$wd"
    mkdir -p "$rd" "$wd"
    cp fixture/bench.ipynb fixture/bench.py "$wd/"
    cf="$rd/cidfile"

    echo "== $stack run$run_index =="
    vol="$(pwd)/$wd:/work:rw"
    container_start_ms=""

    # The conflict scenarios rewrite the notebook from the HOST (the "VS
    # Code save"), so stacks that participate must not leave root-owned
    # files behind: run them as the host user with a writable HOME.
    # datalayer is the exception: jupyter-collaboration/RTC misbehaves as
    # non-root (measured 2026-10-15: every mutating tool timed out), so it
    # runs as root and the driver performs its external write through
    # `docker exec` instead (same bytes on disk, another writer).
    user_flags=()
    case "$stack" in
        tpmcp | cursor | block)
            user_flags=(--user "$(id -u):$(id -g)" -e HOME=/work)
            ;;
    esac

    if is_http "$stack"; then
        port="${MARIMO_PORT:-27333}"
        name="bench-$stack-$run_index"
        t0=$(date +%s%3N)
        docker run -d --rm --name "$name" --cidfile "$cf" \
            -p "127.0.0.1:$port:2718" -v "$vol" "bench-$stack:pin" >/dev/null
        i=0
        until curl -sf -o /dev/null "http://127.0.0.1:$port/"; do
            i=$((i + 1))
            if [ "$i" -gt 120 ]; then
                echo "  marimo HTTP never came up; logs:" >&2
                docker logs "$name" 2>&1 | tail -20 >&2
                docker rm -f "$name" >/dev/null 2>&1 || true
                return 1
            fi
            sleep 1
        done
        t1=$(date +%s%3N)
        container_start_ms=$((t1 - t0))
        echo "  container ready in ${container_start_ms} ms"
        driver_args=(python3 driver.py --stack "$stack"
            --url "http://127.0.0.1:$port/mcp/server")
    else
        case "$stack" in
            cursor)
                server_cmd=(docker run -i --rm --name "bench-$stack-$run_index"
                    "${user_flags[@]}" --cidfile "$cf" -v "$vol"
                    "bench-$stack:pin" --allow-root /work)
                ;;
            analyzer)
                # The official image's default workdir /app is not writable
                # by its own non-root user, so ruff's cache init fails and
                # every analysis tool errors out. A writable workdir plus
                # RUFF_CACHE_DIR is required (finding for COMPARISON.md).
                server_cmd=(docker run -i --rm --name "bench-$stack-$run_index"
                    --cidfile "$cf" -w /tmp
                    -e RUFF_CACHE_DIR=/tmp/ruff-cache
                    "bench-$stack:pin")
                ;;
            yzfly)
                server_cmd=(docker run -i --rm --name "bench-$stack-$run_index"
                    --cidfile "$cf" -v "$vol"
                    "bench-$stack:pin" --dir /work)
                ;;
            *)
                server_cmd=(docker run -i --rm --name "bench-$stack-$run_index"
                    "${user_flags[@]}" --cidfile "$cf" -v "$vol"
                    "bench-$stack:pin")
                ;;
        esac
        driver_args=(python3 driver.py --stack "$stack")
    fi

    driver_args+=(--status "$rd/status" --control "$rd/control"
        --ops-out "$rd/ops.json" --fixture "$wd/bench.ipynb" --cidfile "$cf")

    sampler_args=(python3 sampler.py --stack "$stack" --cidfile "$cf"
        --status "$rd/status" --control "$rd/control" --out "$rd")
    if $fast; then
        sampler_args+=(--fast)
    fi

    set +e
    "${sampler_args[@]}" >"$rd/sampler.log" 2>&1 &
    sampler_pid=$!
    if is_http "$stack"; then
        "${driver_args[@]}" >"$rd/driver.log" 2>&1
    else
        "${driver_args[@]}" -- "${server_cmd[@]}" >"$rd/driver.log" 2>&1
    fi
    driver_rc=$?
    wait "$sampler_pid"
    sampler_rc=$?
    set -e

    if is_http "$stack"; then
        docker rm -f "bench-$stack-$run_index" >/dev/null 2>&1 || true
    fi
    # Orphan guard: killing the docker CLI does not always stop the
    # container; make sure nothing named bench-* survives the run.
    docker rm -f "bench-$stack-$run_index" >/dev/null 2>&1 || true

    size=$(docker image inspect --format '{{.Size}}' "bench-$stack:pin")
    image_id=$(docker image inspect --format '{{.Id}}' "bench-$stack:pin")
    read -r load1 load5 load15 load_rest < /proc/loadavg
    mem_avail=""
    while read -r key val unit; do
        if [ "$key" = "MemAvailable:" ]; then
            mem_avail="$val"
            break
        fi
    done < /proc/meminfo
    {
        echo "{"
        echo "  \"stack\": \"$stack\","
        echo "  \"run\": $run_index,"
        echo "  \"image_size_bytes\": $size,"
        echo "  \"image_id\": \"$image_id\","
        if [ -n "$container_start_ms" ]; then
            echo "  \"container_start_ms\": $container_start_ms,"
        fi
        echo "  \"mode\": \"$(if $fast; then echo fast; else echo normal; fi)\","
        echo "  \"date_utc\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\","
        echo "  \"docker_version\": \"$(docker version --format '{{.Server.Version}}')\","
        echo "  \"host_kernel\": \"$(uname -sr)\","
        echo "  \"loadavg_1m\": $load1,"
        echo "  \"mem_available_kb\": ${mem_avail:-null},"
        echo "  \"driver_rc\": $driver_rc,"
        echo "  \"sampler_rc\": $sampler_rc"
        echo "}"
    } >"$rd/meta.json"

    if [ "$driver_rc" -ne 0 ] || [ "$sampler_rc" -ne 0 ]; then
        echo "  FAILED (driver=$driver_rc sampler=$sampler_rc) - see $rd/*.log"
        return 1
    fi
    echo "  ok - results in $rd"
}

for stack in $stacks; do
    if $do_build; then
        echo "-- building $stack"
        build_stack "$stack"
    fi
    n=1
    while [ "$n" -le "$runs" ]; do
        run_one "$stack" "$n" || echo "  (run failed, continuing)"
        n=$((n + 1))
    done
done

echo
python3 render.py --print-summary results/ || true
