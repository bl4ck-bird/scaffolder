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

Questions can be typed as `string`, `int`, `float`, `boolean`, `select`, or
`multiselect`; answers keep their declared type through rendering.

Apply a template to a new directory:

```sh
scaffolder apply my-template ./demo --answers project=demo
```

The template argument is either a path to a local template directory or the
name of a template in a store. A name is looked up as
`<store>/<name>/scaffold.toml`, searching, in order: the `--template-dir`
override, `$SCAFFOLDER_HOME`, `$XDG_CONFIG_HOME/scaffolder`, and
`~/.scaffolder`.

Answers for any question without a supplied value fall back to its
`default`; a question with no default and no supplied answer is an error,
unless you're at an interactive terminal, in which case you'll be prompted
for it.

A question may carry a `when` expression referencing earlier answers (for
example `when = "'ci' in stacks"`); when it evaluates false the question is
skipped and its default, if any, is used.

A `.scaffoldignore` file (or `.scaffoldignore.jinja`, rendered with the
answers) lists gitignore-style patterns for output paths to leave out of
the generated project.

### Flags

| flag | |
|---|---|
| `--name <n>` | value for the `scaffolder.name` template variable (default: target directory basename) |
| `--answers K=V` | answer a question non-interactively; repeatable |
| `--answers-file <path>` | TOML file of answers (`name = value`); a matching `--answers K=V` takes precedence over the same key here |
| `--defaults` | use each question's default without prompting; fails if a question has no default |
| `--force` | overwrite existing files in the target without prompting |
| `--dry-run` | print the write plan without touching the filesystem |
| `--template-dir <path>` | directory to resolve a template name against, before the default store locations |

Running without `--force` against an existing file fails unless you are at
an interactive terminal and confirm the overwrite.
