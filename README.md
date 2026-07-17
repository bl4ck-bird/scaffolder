# scaffolder

A declarative project scaffolding CLI. It renders a template directory
(`scaffold.toml` + `files/`) into a new project, substituting answers you
provide into any file named with a `.jinja` suffix and copying the rest
verbatim.

## Why

Most scaffolding tools either hide their logic behind opaque generators or
require a full scripting runtime. `scaffolder` is a single native binary
with no runtime dependencies: a template is just a directory of files plus
a small TOML manifest describing the questions it needs answered.

## Install

Build from source with Cargo:

```sh
cargo build --release
```

The binary is written to `target/release/scaffolder`.

## Usage

A template is a directory containing a `scaffold.toml` manifest and a
`files/` directory with the payload to render:

```
my-template/
  scaffold.toml
  files/
    README.md.jinja
    src/main.rs
```

`scaffold.toml` declares the questions the template needs:

```toml
[[questions]]
name = "project"
type = "string"

[[questions]]
name = "license"
type = "string"
default = "MIT"
```

Files ending in `.jinja` are rendered with the answers (and are written
without the `.jinja` suffix); every other file is copied byte-for-byte.

Apply a template to a new directory:

```sh
scaffolder apply my-template ./demo --answers project=demo
```

Answers for any question without a supplied value fall back to its
`default`; a question with no default and no supplied answer is an error.

### Flags

| flag | |
|---|---|
| `--name <n>` | value for the `scaffolder.name` template variable (default: target directory basename) |
| `--answers K=V` | answer a question non-interactively; repeatable |
| `--force` | overwrite existing files in the target without prompting |
| `--dry-run` | print the write plan without touching the filesystem |

Running without `--force` against an existing file fails unless you are at
an interactive terminal and confirm the overwrite.
