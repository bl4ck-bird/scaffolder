# scaffolder

> Scaffold new projects from declarative, reusable templates.

[![CI](https://github.com/bl4ck-bird/scaffolder/actions/workflows/ci.yml/badge.svg)](https://github.com/bl4ck-bird/scaffolder/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/bl4ck-bird/scaffolder?sort=semver)](https://github.com/bl4ck-bird/scaffolder/releases)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

`scaffolder` turns a folder of files plus a small TOML manifest into a new
project. You describe the questions a template needs and mark which files
should be rendered; `scaffolder apply` fills them in — interactively or from
the command line.

```sh
scaffolder apply rust-starter ./my-app --answers project=my-app
```

## Features

- **Templates are just files.** A template is a directory with a
  `scaffold.toml` manifest and a `files/` payload — there is no template DSL
  to learn.
- **Type-preserving rendering.** Files ending in `.jinja` are rendered with
  your answers (via [MiniJinja](https://github.com/mitsuhiko/minijinja));
  everything else is copied byte-for-byte. Answers keep their declared type
  (`string`, `int`, `float`, `boolean`, `select`, `multiselect`) all the way
  through rendering.
- **Interactive or scripted.** Answer questions at a prompt, or pass them
  with `--answers` / `--answers-file` for fully non-interactive runs.
- **Conditional questions and files.** A `when` expression can skip a
  question based on earlier answers, and `.scaffoldignore` can leave files
  out of the generated project.
- **Partials, data, and hooks.** Reuse snippets with `{% include %}`, expose
  static values as `data.*`, and run `before` / `after` shell hooks (gated
  behind a confirmation prompt).
- **Safe by default.** Writes stay inside the target, overwrites and
  out-of-target writes require confirmation, and each file is written
  atomically. If a run fails partway, the target it created is cleaned up.
- **A single binary.** No runtime or interpreter to install.

## Installation

Install the latest release with the one-line installer (Linux and macOS):

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/bl4ck-bird/scaffolder/releases/latest/download/scaffolder-installer.sh | sh
```

It downloads a prebuilt binary for your platform and places it in Cargo's `bin`
directory (`~/.cargo/bin` by default). You can also grab a binary straight from
the [releases page](https://github.com/bl4ck-bird/scaffolder/releases).

Or build from source with Cargo:

```sh
cargo build --release   # binary at target/release/scaffolder
```

Or install straight from the repository:

```sh
cargo install --git https://github.com/bl4ck-bird/scaffolder
```

## Quick start

A template is a directory with a manifest and a payload:

```
rust-starter/
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

Apply it to a new directory:

```sh
scaffolder apply rust-starter ./my-app --answers project=my-app
```

`files/README.md.jinja` is rendered — `{{ project }}` becomes `my-app` — and
written as `README.md`, while `src/main.rs` is copied unchanged.

The `examples/rust-starter/` template in this repository is ready to run:

```sh
scaffolder apply examples/rust-starter ./demo --answers stacks=docker,ci
```

It shows off conditional files (a `multiselect` toggling `Dockerfile` and CI
files), a partial deduplicated across `.gitignore`, `executable_` mode bits on
a build script, and a GitHub Actions workflow whose `${{ matrix.os }}` syntax
passes through untouched because that file has no `.jinja` suffix.

## How templates work

### Rendering and file names

Files ending in `.jinja` are rendered with your answers and written **without**
the suffix. Every other file is copied verbatim, so literal `${{ ... }}` syntax
(in a CI workflow, say) passes through untouched.

A file's basename may carry stackable Unix **mode prefixes** — `executable_`,
`private_`, and `readonly_`, in any order (e.g. `executable_private_deploy.sh`).
They set the corresponding permission bits on the written file and are stripped
from the output name. Mode prefixes are Unix-only.

### Questions and answers

Questions are typed `string`, `int`, `float`, `boolean`, `select`, or
`multiselect`. For each question, the value is taken from the first of these
that applies:

1. a matching `--answers K=V` on the command line,
2. a matching key in `--answers-file`,
3. the question's `default` (with `--defaults`, or non-interactively),
4. an interactive prompt, when you are at a terminal.

A question with no supplied value and no `default` is an error, unless you can
be prompted for it. A `when` expression can gate a question on earlier answers
— for example `when = "'ci' in stacks"` — and when it is false the question is
skipped and its default, if any, is used.

### Leaving files out

A `.scaffoldignore` file (or `.scaffoldignore.jinja`, rendered with your
answers) lists gitignore-style patterns for output paths to omit from the
generated project.

### Finding a template

The template argument is either a path to a local template directory or the
name of a template in a **store**. A name is looked up as
`<store>/<name>/scaffold.toml`, searching in order: the `--template-dir`
override, `$SCAFFOLDER_HOME`, `$XDG_CONFIG_HOME/scaffolder`, and
`~/.scaffolder`.

### Partials, data, and hooks

A template can also include:

- **`partials/`** — reusable snippets pulled into any rendered file with
  `{% include "name" %}`. Partials are always rendered, even when the file
  including them has no `.jinja` suffix of its own.
- **`data/*.toml`** — static TOML loaded without rendering and exposed to
  every template file as `data.*` (a `[data]` table in `scaffold.toml` merges
  into the same namespace).
- **Hooks** — shell commands run around the write phase. Declare inline
  commands with `[[hooks.before]]` / `[[hooks.after]]` `run = "..."` in
  `scaffold.toml`, and/or drop scripts into `hooks/before/` and `hooks/after/`
  (executable files run as-is; scripts with a `.jinja` extension are rendered
  first). Hook execution is gated behind a confirmation prompt; pass `--yes` to
  run them without it.

## Managing templates

Beyond `apply`, the `scaffolder template` subcommands manage a store of
templates (using the same lookup order as `apply`):

```sh
scaffolder template list [--template-dir <path>]
scaffolder template new <name> [--full] [--template-dir <path>]
scaffolder template validate [names...] [--template-dir <path>]
```

- **`list`** prints every template found across the store locations. If the
  same name exists in more than one, each occurrence is shown with its base
  path.
- **`new <name>`** scaffolds a fresh template skeleton in the highest-priority
  store. The default is a minimal valid template; `--full` also adds
  `partials/`, `data/`, and `hooks/` samples. It fails if the name already
  exists.
- **`validate [names...]`** statically checks templates — manifest schema,
  `when` expressions, file-name grammar, partial references, `.jinja` syntax,
  and output-name conflicts. With no names it validates the whole store;
  problems are grouped by kind and the command exits non-zero.

## `apply` flags

| Flag | Description |
|---|---|
| `--name <n>` | value for the `scaffolder.name` template variable (default: the target directory's basename) |
| `--answers K=V` | answer a question non-interactively; repeatable |
| `--answers-file <path>` | TOML file of answers (`name = value`); a matching `--answers K=V` wins over the same key here |
| `--defaults` | use each question's default without prompting; fails if a question has no default |
| `--force` | overwrite existing files in the target without prompting |
| `--yes` | run the template's hooks without the confirmation prompt |
| `--trust` | allow reading control files reached by a symlink pointing outside the template (default: refuse) |
| `--dry-run` | print the write plan without touching the filesystem |
| `--template-dir <path>` | directory to resolve a template name against, before the default store locations |
| `--no-cleanup-on-failure` | keep a newly created target if `apply` fails partway (default: it is removed) |

Running without `--force` against an existing file fails unless you are at a
terminal and confirm the overwrite. If `apply` creates a new target and then
fails, that target is removed so no half-written project is left behind (a
pre-existing target is always preserved); pass `--no-cleanup-on-failure` to
keep the partial output instead. Cleanup is best-effort and does not cover
interruption by a signal, `SIGKILL`, or power loss.

## Development

Git hooks are managed with [lefthook](https://github.com/evilmartians/lefthook).
After cloning, install the hook manager and the tools the hooks invoke, then
wire them into the repo:

```sh
brew install lefthook typos-cli      # see each tool's docs for other package managers
rustup component add rustfmt clippy  # usually already present with the toolchain
lefthook install
```

The hooks then run automatically:

- **pre-commit** — `cargo fmt`, `cargo clippy`, and
  [`typos`](https://github.com/crate-ci/typos).
- **pre-push** — the full `cargo test` suite.

[cargo-deny](https://github.com/EmbarkStudios/cargo-deny) (advisories, licenses,
bans, sources) runs in CI rather than a hook; to run it locally:
`cargo install cargo-deny && cargo deny check`.

## License

Licensed under either of [MIT](LICENSE-MIT) or
[Apache License, Version 2.0](LICENSE-APACHE) at your option.
