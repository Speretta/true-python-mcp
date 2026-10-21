# Library docs for a dotted symbol and a module

The user is about to write code against `json`, `pathlib` and `antigravity`,
and wants the AI to look up the exact signatures and docstrings — what a
function takes and returns, what a class inherits from, what doctest examples
a docstring carries, and what happens when a definition has no docstring at
all.

> **User asks the AI:** *"What does `json.dumps` take and return? What does
> `pathlib.Path` inherit from? And what members does `json` export?"*

What follows is the real conversation between the AI (the LLM) and tpmcp,
captured from the server. The LLM sends tool calls and sees the answers as
markdown (or JSON, when it asks for it).

## Step 1: Resolve `json.dumps` (`py_docs`)

`py_docs` resolves a dotted symbol to its kind, rendered signature, structured
parameters, docstring and a hop-by-hop resolution trace — fully local, using
ty's IDE support on the vendored typeshed, no interpreter, no subprocess. The
LLM sends:

````json
{
  "name": "py_docs",
  "arguments": {
    "symbol": "json.dumps"
  }
}
````

tpmcp answers:

````markdown
py_docs: json.dumps
resolved_name: dumps
kind: function (unique)
signature: def dumps(obj: Any, *, skipkeys: bool = False, ensure_ascii: bool = True, check_circular: bool = True, allow_nan: bool = True, cls: type[JSONEncoder] | None = None, indent: None | int | str = None, separators: tuple[str, str] | None = None, default: ((Any, /) -> Any) | None = None, sort_keys: bool = False, **kwds: Any) -> str
return_type: str
parameters:
  obj: Any (positional_or_keyword)
  skipkeys: bool = False (keyword_only)
  ensure_ascii: bool = True (keyword_only)
  check_circular: bool = True (keyword_only)
  allow_nan: bool = True (keyword_only)
  cls: type[JSONEncoder] | None = None (keyword_only)
  indent: None | int | str = None (keyword_only)
  separators: tuple[str, str] | None = None (keyword_only)
  default: ((Any, /) -> Any) | None = None (keyword_only)
  sort_keys: bool = False (keyword_only)
  kwds: Any (keyword_variadic)
docstring (stub):
Serialize ``obj`` to a JSON formatted ``str``.

    If ``skipkeys`` is true then ``dict`` keys that are not basic types
    (``str``, ``int``, ``float``, ``bool``, ``None``) will be skipped
    instead of raising a ``TypeError``.

    If ``ensure_ascii`` is false, then the return value can contain
    non-ASCII and non-printable characters if they appear in strings
    contained in ``obj``.  Otherwise, all such characters are escaped in
    JSON strings.

    If ``check_circular`` is false, then the circular reference check
    for container types will be skipped and a circular reference will
    result in an ``RecursionError`` (or worse).

    If ``allow_nan`` is false, then it will be a ``ValueError`` to
    serialize out of range ``float`` values (``nan``, ``inf``, ``-inf``) in
    strict compliance of the JSON specification, instead of using the
    JavaScript equivalents (``NaN``, ``Infinity``, ``-Infinity``).

    If ``indent`` is a non-negative integer, then JSON array elements and
    object members will be pretty-printed with that indent level. An indent
    level of 0 will only insert newlines. ``None`` is the default and gives
    a representation with no newlines inserted.

    If specified, ``separators`` should be an ``(item_separator,
    key_separator)`` tuple.  The default is ``(', ', ': ')`` if *indent* is
    ``None`` and ``(',', ': ')`` otherwise.  To get the most compact JSON
    representation, you should specify ``(',', ':')`` to eliminate
    whitespace.

    ``default(obj)`` is a function that should return a serializable version
    of obj or raise TypeError. The default simply raises TypeError.

    If *sort_keys* is true (default: ``False``), then the output of
    dictionaries will be sorted by key.

    To use a custom ``JSONEncoder`` subclass (e.g. one that overrides the
    ``.default()`` method to serialize additional types), specify it with
    the ``cls`` kwarg; otherwise ``JSONEncoder`` is used.

    
source: stdlib/json/__init__.pyi:109
is_stub: true
resolution:
  json -> module json
  dumps -> function json.dumps
````

The answer names the terminal definition (`resolved_name: dumps`), its coarse
`kind` (`function`, disambiguated as `unique`), the full rendered signature,
and the signature split into structured `parameters` — each with its
`positional_or_keyword` / `keyword_only` kind and default. The docstring comes
from the vendored typeshed stub (`docstring (stub)`), and the `resolution`
trace records each hop: `json` is a module, `dumps` a function on it.

## Step 2: The same symbol as JSON (`py_docs`, `format=json`)

Every tool speaks both formats. The same lookup with `format: json` returns
the structured content — the same fields, plus `source_line` and the (empty)
`doctest_examples` list:

````json
{
  "name": "py_docs",
  "arguments": {
    "symbol": "json.dumps",
    "format": "json"
  }
}
````

tpmcp answers (structuredContent):

````json
{"bases":[],"candidates":[],"disambiguation":"unique","docstring":"Serialize ``obj`` to a JSON formatted ``str``.\n\n    If ``skipkeys`` is true then ``dict`` keys that are not basic types\n    (``str``, ``int``, ``float``, ``bool``, ``None``) will be skipped\n    instead of raising a ``TypeError``.\n\n    If ``ensure_ascii`` is false, then the return value can contain\n    non-ASCII and non-printable characters if they appear in strings\n    contained in ``obj``.  Otherwise, all such characters are escaped in\n    JSON strings.\n\n    If ``check_circular`` is false, then the circular reference check\n    for container types will be skipped and a circular reference will\n    result in an ``RecursionError`` (or worse).\n\n    If ``allow_nan`` is false, then it will be a ``ValueError`` to\n    serialize out of range ``float`` values (``nan``, ``inf``, ``-inf``) in\n    strict compliance of the JSON specification, instead of using the\n    JavaScript equivalents (``NaN``, ``Infinity``, ``-Infinity``).\n\n    If ``indent`` is a non-negative integer, then JSON array elements and\n    object members will be pretty-printed with that indent level. An indent\n    level of 0 will only insert newlines. ``None`` is the default and gives\n    a representation with no newlines inserted.\n\n    If specified, ``separators`` should be an ``(item_separator,\n    key_separator)`` tuple.  The default is ``(', ', ': ')`` if *indent* is\n    ``None`` and ``(',', ': ')`` otherwise.  To get the most compact JSON\n    representation, you should specify ``(',', ':')`` to eliminate\n    whitespace.\n\n    ``default(obj)`` is a function that should return a serializable version\n    of obj or raise TypeError. The default simply raises TypeError.\n\n    If *sort_keys* is true (default: ``False``), then the output of\n    dictionaries will be sorted by key.\n\n    To use a custom ``JSONEncoder`` subclass (e.g. one that overrides the\n    ``.default()`` method to serialize additional types), specify it with\n    the ``cls`` kwarg; otherwise ``JSONEncoder`` is used.\n\n    ","docstring_source":"stub","doctest_examples":[],"is_reexported":false,"is_stub":true,"kind":"function","note":null,"parameters":[{"default":null,"kind":"positional_or_keyword","name":"obj","type":"Any"},{"default":"False","kind":"keyword_only","name":"skipkeys","type":"bool"},{"default":"True","kind":"keyword_only","name":"ensure_ascii","type":"bool"},{"default":"True","kind":"keyword_only","name":"check_circular","type":"bool"},{"default":"True","kind":"keyword_only","name":"allow_nan","type":"bool"},{"default":"None","kind":"keyword_only","name":"cls","type":"type[JSONEncoder] | None"},{"default":"None","kind":"keyword_only","name":"indent","type":"None | int | str"},{"default":"None","kind":"keyword_only","name":"separators","type":"tuple[str, str] | None"},{"default":"None","kind":"keyword_only","name":"default","type":"((Any, /) -> Any) | None"},{"default":"False","kind":"keyword_only","name":"sort_keys","type":"bool"},{"default":null,"kind":"keyword_variadic","name":"kwds","type":"Any"}],"resolution":["json -> module json","dumps -> function json.dumps"],"resolved_name":"dumps","return_type":"str","signatures":["def dumps(obj: Any, *, skipkeys: bool = False, ensure_ascii: bool = True, check_circular: bool = True, allow_nan: bool = True, cls: type[JSONEncoder] | None = None, indent: None | int | str = None, separators: tuple[str, str] | None = None, default: ((Any, /) -> Any) | None = None, sort_keys: bool = False, **kwds: Any) -> str"],"source":"stdlib/json/__init__.pyi","source_line":109,"symbol":"json.dumps"}
````

The JSON repeats the docstring as one escaped string and carries, as
structured data, exactly what the markdown renders line-by-line.
`doctest_examples` is `[]` here because `dumps`'s docstring embeds no doctest.

## Step 3: A class's bases (`pathlib.Path`)

For a class, `py_docs` reports its direct superclasses in `bases` and renders
the constructor (so the LLM learns how to instantiate it):

````json
{
  "name": "py_docs",
  "arguments": {
    "symbol": "pathlib.Path"
  }
}
````

tpmcp answers:

````markdown
py_docs: pathlib.Path
resolved_name: Path
kind: class (unique)
bases: PurePath
signature: class Path(
    *args: str | PathLike[str],
    **kwargs: object
)
docstring (stub):
PurePath subclass that can make system calls.

    Path represents a filesystem path but unlike PurePath, also offers
    methods to do system calls on path objects. Depending on your system,
    instantiating a Path will return either a PosixPath or a WindowsPath
    object. You can also instantiate a PosixPath or WindowsPath directly,
    but cannot instantiate a WindowsPath on a POSIX system or vice versa.
    
source: stdlib/pathlib/__init__.pyi:307
is_stub: true
resolution:
  pathlib -> module pathlib
  Path -> class pathlib.Path
````

`bases: PurePath` names the direct superclass, and the `class Path(...)`
signature is the constructor.

## Step 4: Docstring enrichment from the real interpreter (`antigravity.geohash`)

`antigravity` is a stdlib stub whose `geohash` docstring the vendored typeshed
`antigravity.pyi` omits. `py_docs` falls back to the interpreter's *real*
CPython `Lib/antigravity.py`, so this step pins `python` to a real interpreter
— the ground-truth venv — rather than relying on auto-discovery:

````json
{
  "name": "py_docs",
  "arguments": {
    "symbol": "antigravity.geohash",
    "python": "/tmp/opencode/nbenv/bin/python"
  }
}
````

tpmcp answers:

````markdown
py_docs: antigravity.geohash
resolved_name: geohash
kind: function (unique)
signature: def geohash(latitude: float, longitude: float, datedow: Buffer) -> None
return_type: None
parameters:
  latitude: float (positional_or_keyword)
  longitude: float (positional_or_keyword)
  datedow: Buffer (positional_or_keyword)
docstring (source):
Compute geohash() using the Munroe algorithm.

    >>> geohash(37.421542, -122.085589, b'2005-05-26-10458.68')
    37.857713 -122.544543

    
doctests:
  >>> geohash(37.421542, -122.085589, b'2005-05-26-10458.68')
  37.857713 -122.544543
source: stdlib/antigravity.pyi:3
is_stub: true
resolution:
  antigravity -> module antigravity
  geohash -> function antigravity.geohash
````

`docstring (source)` marks the enriched docstring. It also embeds one doctest
example, so the answer carries a `doctests:` section alongside it.

## Step 5: The honest note when there is no docstring (`no_docs.f`)

`examples/demo/no_docs.py` defines `f` with no docstring. `py_docs` reports
`docstring: none` plus a `note` naming what was looked at — never a silent
empty answer:

````json
{
  "name": "py_docs",
  "arguments": {
    "symbol": "no_docs.f",
    "path": "examples/demo"
  }
}
````

tpmcp answers:

````markdown
py_docs: no_docs.f
resolved_name: f
kind: function (unique)
signature: def f() -> Unknown
return_type: Unknown
docstring: none
note: no docstring: the definition has none (C extension or bare stub)
source: /media/oplta/ssd/Projects/true-python-mcp/wt-remaining/examples/demo/no_docs.py:1
resolution:
  no_docs -> module no_docs
  f -> function no_docs.f
````

(The `source:` line is the absolute path on the capture machine; on yours it
names your own checkout.)

## Step 6: Enumerate a module's members (`py_module_members`)

`py_module_members` lists a module's public surface — kind, one-line signature
and one-line docstring per member, public names first, then dunders:

````json
{
  "name": "py_module_members",
  "arguments": {
    "module": "json"
  }
}
````

tpmcp answers:

````markdown
py_module_members: json
resolved_name: json
member_count: 43
members:
  JSONDecodeError: class
    signature: class JSONDecodeError(...)
    docstring: Subclass of ValueError with the following additional properties:
  JSONDecoder: class
    signature: class JSONDecoder( *, object_hook: ((dict[str, Any], /) -> Any) | None = None, parse_float: ((str, /) -> Any) | None = None, parse_int: ((str, /) -> Any) | None = None, parse_constant: ((str, /) -> Any) | None = None, strict: bool = True, object_pairs_hook: ((list[tuple[str, Any]], /) -> Any) | None = None )
    docstring: Simple JSON <https://json.org> decoder
  JSONEncoder: class
    signature: class JSONEncoder( *, skipkeys: bool = False, ensure_ascii: bool = True, check_circular: bool = True, allow_nan: bool = True, sort_keys: bool = False, indent: int | str | None = None, separators: tuple[str, str] | None = None, default: ((...) -> Any) | None = None )
    docstring: Extensible JSON <https://json.org> encoder for Python data structures.
  decoder: module
    note: no docstring: the stub has none and the CPython source `/usr/lib/python3.14/json/__init__.py` has none to fall back to (looked at: `stdlib/json/__init__.pyi`, `/usr/lib/python3.14/json/__init__.py`)
  detect_encoding: function
    signature: def detect_encoding(b: bytes | bytearray) -> Literal["utf-8", "utf-8-sig", "utf-16", "utf-16-be", "utf-16-le", ... omitted 3 literals]
    note: no docstring: the stub has none and the CPython source `/usr/lib/python3.14/json/__init__.py` has none to fall back to (looked at: `stdlib/json/__init__.pyi`, `/usr/lib/python3.14/json/__init__.py`)
  dump: function
    signature: def dump(obj: Any, fp: SupportsWrite[str], *, skipkeys: bool = False, ensure_ascii: bool = True, check_circular: bool = True, allow_nan: bool = True, cls: type[JSONEncoder] | None = None, indent: None | int | str = None, separators: tuple[str, str] | None = None, default: ((Any, /) -> Any) | None = None, sort_keys: bool = False, **kwds: Any) -> None
    docstring: Serialize ``obj`` as a JSON formatted stream to ``fp`` (a
  dumps: function
    signature: def dumps(obj: Any, *, skipkeys: bool = False, ensure_ascii: bool = True, check_circular: bool = True, allow_nan: bool = True, cls: type[JSONEncoder] | None = None, indent: None | int | str = None, separators: tuple[str, str] | None = None, default: ((Any, /) -> Any) | None = None, sort_keys: bool = False, **kwds: Any) -> str
    docstring: Serialize ``obj`` to a JSON formatted ``str``.
  encoder: module
    note: no docstring: the stub has none and the CPython source `/usr/lib/python3.14/json/__init__.py` has none to fall back to (looked at: `stdlib/json/__init__.pyi`, `/usr/lib/python3.14/json/__init__.py`)
  load: function
    signature: def load(fp: SupportsRead[str | bytes], *, cls: type[JSONDecoder] | None = None, object_hook: ((dict[Any, Any], /) -> Any) | None = None, parse_float: ((str, /) -> Any) | None = None, parse_int: ((str, /) -> Any) | None = None, parse_constant: ((str, /) -> Any) | None = None, object_pairs_hook: ((list[tuple[Any, Any]], /) -> Any) | None = None, **kwds: Any) -> Any
    docstring: Deserialize ``fp`` (a ``.read()``-supporting file-like object containing
  loads: function
    signature: def loads(s: str | bytes | bytearray, *, cls: type[JSONDecoder] | None = None, object_hook: ((dict[Any, Any], /) -> Any) | None = None, parse_float: ((str, /) -> Any) | None = None, parse_int: ((str, /) -> Any) | None = None, parse_constant: ((str, /) -> Any) | None = None, object_pairs_hook: ((list[tuple[Any, Any]], /) -> Any) | None = None, **kwds: Any) -> Any
    docstring: Deserialize ``s`` (a ``str``, ``bytes`` or ``bytearray`` instance
  scanner: other
    note: member `scanner` has no ranked definition
  tool: other
    note: member `tool` has no ranked definition
  __all__: attribute
    note: no docstring: the stub has none and the CPython source `/usr/lib/python3.14/json/__init__.py` has none to fall back to (looked at: `stdlib/json/__init__.pyi`, `/usr/lib/python3.14/json/__init__.py`)
  __annotations__: other
    note: member `__annotations__` has no ranked definition
  __class__: other
    note: member `__class__` has no ranked definition
  __delattr__: other
    note: member `__delattr__` has no ranked definition
  __dict__: other
    note: member `__dict__` has no ranked definition
  __dir__: other
    note: member `__dir__` has no ranked definition
  __doc__: other
    note: member `__doc__` has no ranked definition
  __eq__: other
    note: member `__eq__` has no ranked definition
  __file__: other
    note: member `__file__` has no ranked definition
  __format__: other
    note: member `__format__` has no ranked definition
  __getattr__: other
    note: member `__getattr__` has no ranked definition
  __getattribute__: other
    note: member `__getattribute__` has no ranked definition
  __getstate__: other
    note: member `__getstate__` has no ranked definition
  __hash__: other
    note: member `__hash__` has no ranked definition
  __init__: other
    note: member `__init__` has no ranked definition
  __init_subclass__: other
    note: member `__init_subclass__` has no ranked definition
  __loader__: other
    note: member `__loader__` has no ranked definition
  __module__: other
    note: member `__module__` has no ranked definition
  __name__: other
    note: member `__name__` has no ranked definition
  __ne__: other
    note: member `__ne__` has no ranked definition
  __new__: other
    note: member `__new__` has no ranked definition
  __package__: other
    note: member `__package__` has no ranked definition
  __path__: other
    note: member `__path__` has no ranked definition
  __reduce__: other
    note: member `__reduce__` has no ranked definition
  __reduce_ex__: other
    note: member `__reduce_ex__` has no ranked definition
  __repr__: other
    note: member `__repr__` has no ranked definition
  __setattr__: other
    note: member `__setattr__` has no ranked definition
  __sizeof__: other
    note: member `__sizeof__` has no ranked definition
  __spec__: other
    note: member `__spec__` has no ranked definition
  __str__: other
    note: member `__str__` has no ranked definition
  __subclasshook__: other
    note: member `__subclasshook__` has no ranked definition
````

The listing is honest where it cannot fill a field. `decoder`/`encoder`/`detect_encoding`
are modules with no docstring in the stub or in the interpreter's source (the
`/usr/lib/python3.14/` paths are the capture machine's system interpreter),
and `scanner`/`tool` are names ty enumerated but could not re-resolve.

## Result

Nothing changed on disk: `py_docs` and `py_module_members` are read-only. The
AI now knows `json.dumps`'s full signature and docstring, `pathlib.Path`'s
superclass, `antigravity.geohash`'s enriched docstring and doctest, the honest
`no_docs.f` note, and `json`'s member listing.

## The input files

`examples/demo/no_docs.py` before the session:

````python
def f():
    pass
````

