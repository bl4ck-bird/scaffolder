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

Or install straight from the repository:

```sh
cargo install --git https://github.com/bl4ck-bird/scaffolder
```

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
without the `.jinja` suffix); every other file is copied byte-for-byte. A
file whose basename starts with `executable_`, `private_`, and/or
`readonly_` (stackable, in any order, e.g. `executable_private_deploy.sh`)
gets the matching Unix permission bits on the written file; the prefix is
stripped from the output name. Mode prefixes are Unix-only.

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

### Partials, data, and hooks

A template can also include:

- `partials/` — reusable snippets pulled into any rendered file with
  `{% include "name" %}`. Partials are always rendered, even when included
  from a file that has no `.jinja` suffix of their own.
- `data/*.toml` — static TOML loaded without rendering and exposed to every
  template file as `data.*` (a `[data]` table in a `data/*.toml` file
  merges into the same namespace).
- Hooks — shell commands run around the write phase. Declare inline
  commands as `[[hooks.before]]` / `[[hooks.after]]` `run = "..."` entries
  in `scaffold.toml`, and/or drop scripts into `hooks/before/` and
  `hooks/after/` (executable files run as-is; scripts with a template
  extension are rendered first). Hook execution is gated behind a
  confirmation prompt; pass `--yes` to `apply` to skip it.

### Flags

| flag | |
|---|---|
| `--name <n>` | value for the `scaffolder.name` template variable (default: target directory basename) |
| `--answers K=V` | answer a question non-interactively; repeatable |
| `--answers-file <path>` | TOML file of answers (`name = value`); a matching `--answers K=V` takes precedence over the same key here |
| `--defaults` | use each question's default without prompting; fails if a question has no default |
| `--force` | overwrite existing files in the target without prompting |
| `--yes` | run the template's hooks without the confirmation prompt |
| `--trust` | allow reading control files reached by a symlink that points outside the template (default: refuse) |
| `--dry-run` | print the write plan without touching the filesystem |
| `--template-dir <path>` | directory to resolve a template name against, before the default store locations |

Running without `--force` against an existing file fails unless you are at
an interactive terminal and confirm the overwrite.

### Example

`examples/rust-starter/` is a working template you can apply directly:

```sh
scaffolder apply examples/rust-starter ./demo --answers stacks=docker,ci
```

It demonstrates conditional stacks (a `multiselect` question toggling
`Dockerfile`/CI files via `.scaffoldignore.jinja`'s rendered Jinja
conditions), a partial pulled in and deduplicated across `.gitignore.jinja`,
`executable_` mode bits on a build script, and a GitHub Actions workflow
whose `${{ matrix.os }}` syntax passes through untouched because the file
has no `.jinja` suffix.

## Managing the template store

Beyond `apply`, `scaffolder template` manages templates kept in a store
(the same `--template-dir` / `$SCAFFOLDER_HOME` / `$XDG_CONFIG_HOME/scaffolder`
/ `~/.scaffolder` lookup order used by `apply`):

```sh
scaffolder template list [--template-dir <path>]
scaffolder template new <name> [--full] [--template-dir <path>]
scaffolder template validate [names...] [--template-dir <path>]
```

- `list` prints every template found across the store locations; if the
  same name exists in more than one, each occurrence is shown with its
  base path.
- `new <name>` scaffolds a fresh template skeleton in the
  highest-priority store base. Without `--full` you get a minimal valid
  template (`scaffold.toml` + `files/` + a sample file); `--full` adds
  `partials/`, `data/`, and `hooks/` samples on top. Fails if a template
  with that name already exists.
- `validate [names...]` statically checks templates — manifest schema,
  question `when` expressions, file name grammar, partial references,
  `.jinja` syntax, and output-name conflicts. With no names it validates
  every template in the store; problems are reported grouped by kind and
  the command exits non-zero.

## License

Licensed under either of [MIT](LICENSE-MIT) or
[Apache License, Version 2.0](LICENSE-APACHE) at your option.
