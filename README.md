# Zed + VS Code-Style Git UI (Fork)

This fork adds a **VS Code-style Source Control panel** and **side-by-side diff view** to Zed, because we love how VS Code handles Git.

### Why This Fork?

Zed's built-in Git panel groups files by "tracked" vs "untracked" and opens a single combined diff for all changes. We prefer VS Code's approach: separating **Staged Changes** from **Changes** (unstaged), and clicking a file opens a diff for **that file only**. This fork brings that workflow to Zed.

### Key Features

1. **Staged vs Unstaged grouping** — Files are categorized into "Staged Changes" and "Changes" sections, just like VS Code. Partially staged files appear in both. No more guessing what's going into your next commit.

2. **Per-file diff view** — Clicking a file opens a side-by-side diff for that single file. "Changes" shows working copy vs index (`git diff`); "Staged Changes" shows index vs HEAD (`git diff --cached`).

3. **Fast custom diff view** — Zed's built-in `SplittableEditor` builds row-by-row mappings (O(lines)), taking ~1 second for a 6,000-line file and 3–4 seconds for files like `Cargo.lock` (tens of thousands of lines). We replaced it with a custom two-editor implementation using block spacers at hunk boundaries — O(hunks) instead of O(lines), rendering instantly regardless of file size.

4. **Commit history** — Collapsible commit history section pinned to the bottom of the Source Control panel. Click a commit to expand and see its file changes; click a file to open the commit's diff view with alignment blocks. Paginated with "Load More" for large histories. Draggable resize handle between the changes and history sections.

5. **"Compared to [branch]" view** — When on a feature branch, shows aggregate file changes compared to the base branch (like GitHub's "Files changed" tab). Click any file to see the combined diff from the fork point to HEAD. Automatically detects the upstream tracking branch or falls back to the default branch.

### Additional Features

- **Stage and restore buttons** in the divider column between LHS and RHS editors (+ to stage, → to restore)
- **Stage, unstage, and discard buttons** per file in the Source Control panel
- **Draggable resize handle** between LHS and RHS editors (double-click to reset to 50/50)
- **Diff toolbar** with previous/next hunk navigation arrows
- **Open File button** that jumps to the same scroll position in the regular editor
- **Synchronized scrolling** between LHS and RHS
- **Diagonal line pattern spacer blocks** for visual alignment at hunk boundaries
- **Auto-scroll to first change** when opening a diff
- **Proactive diff recalculation** after staging — reduces feedback from ~480ms to ~37ms
- **Custom logo** (Zed + VS Code) to distinguish this build

### Known Issues

- **No diff highlighting on the left side** — The LHS editor shows the old content but does not highlight changed/deleted lines with background colors. Only the RHS has diff decorations.

### Building

This fork uses the **nightly** release channel because the nightly icon looks great.

**Prerequisites:**
```
cargo install cargo-bundle --git https://github.com/zed-industries/cargo-bundle.git --branch zed-deploy
brew install zig
```

**Build and install:**
```
script/bundle-mac -i
```

This creates a release build and installs `Zed Nightly.app` to `/Applications`. Use `-o` instead of `-i` to just open the app without installing.

For a faster debug build (uses existing compilation artifacts):
```
script/bundle-mac -do
```

### Remote Server (SSH)

When connecting to a remote server via SSH, Zed cross-compiles the remote server binary from source using `zig` and uploads it automatically. The binary is cached at `~/.zed_server/` on the remote machine.

This is necessary because our fork adds new RPC messages (for commit history, branch comparison, etc.) that are incompatible with upstream Zed's remote server.

**Protocol versioning:** The remote server binary name uses a protocol version from `crates/remote/REMOTE_SERVER_VERSION` instead of the commit hash. This means:
- **UI-only changes** (panel layout, diff view, etc.) → reuse the cached remote server, no rebuild
- **Proto/RPC changes** (new messages, modified fields) → bump the version number in `REMOTE_SERVER_VERSION` to trigger a rebuild

The cross-compiled binary is built in release mode (~89MB). First-time compilation takes 5-10 minutes depending on your machine.

### Key Crate

All new code lives in `crates/vs_git_ui/` with four main files:
- `vs_git_panel.rs` — the Source Control side panel with changes, history, and compared sections
- `vs_file_diff_view.rs` — the side-by-side diff view for working copy changes
- `vs_commit_diff_view.rs` — the side-by-side diff view for commit and branch comparison diffs
- `vs_diff_toolbar.rs` — the toolbar with navigation and open file

---

# Zed

[![Zed](https://img.shields.io/endpoint?url=https://raw.githubusercontent.com/zed-industries/zed/main/assets/badge/v0.json)](https://zed.dev)
[![CI](https://github.com/zed-industries/zed/actions/workflows/run_tests.yml/badge.svg)](https://github.com/zed-industries/zed/actions/workflows/run_tests.yml)

Welcome to Zed, a high-performance, multiplayer code editor from the creators of [Atom](https://github.com/atom/atom) and [Tree-sitter](https://github.com/tree-sitter/tree-sitter).

---

### Installation

On macOS, Linux, and Windows you can [download Zed directly](https://zed.dev/download) or install Zed via your local package manager ([macOS](https://zed.dev/docs/installation#macos)/[Linux](https://zed.dev/docs/linux#installing-via-a-package-manager)/[Windows](https://zed.dev/docs/windows#package-managers)).

Other platforms are not yet available:

- Web ([tracking issue](https://github.com/zed-industries/zed/issues/5396))

### Developing Zed

- [Building Zed for macOS](./docs/src/development/macos.md)
- [Building Zed for Linux](./docs/src/development/linux.md)
- [Building Zed for Windows](./docs/src/development/windows.md)

### Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for ways you can contribute to Zed.

Also... we're hiring! Check out our [jobs](https://zed.dev/jobs) page for open roles.

### Licensing

License information for third party dependencies must be correctly provided for CI to pass.

We use [`cargo-about`](https://github.com/EmbarkStudios/cargo-about) to automatically comply with open source licenses. If CI is failing, check the following:

- Is it showing a `no license specified` error for a crate you've created? If so, add `publish = false` under `[package]` in your crate's Cargo.toml.
- Is the error `failed to satisfy license requirements` for a dependency? If so, first determine what license the project has and whether this system is sufficient to comply with this license's requirements. If you're unsure, ask a lawyer. Once you've verified that this system is acceptable add the license's SPDX identifier to the `accepted` array in `script/licenses/zed-licenses.toml`.
- Is `cargo-about` unable to find the license for a dependency? If so, add a clarification field at the end of `script/licenses/zed-licenses.toml`, as specified in the [cargo-about book](https://embarkstudios.github.io/cargo-about/cli/generate/config.html#crate-configuration).

## Sponsorship

Zed is developed by **Zed Industries, Inc.**, a for-profit company.

If you’d like to financially support the project, you can do so via GitHub Sponsors.
Sponsorships go directly to Zed Industries and are used as general company revenue.
There are no perks or entitlements associated with sponsorship.
