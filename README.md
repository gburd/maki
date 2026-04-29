<img src="./banner.png">

An AI coding agent optimized for minimal use of context tokens, while providing a great user experience.

Context efficiency:
* `index` tool - uses [tree-sitter](https://tree-sitter.github.io/tree-sitter) to parse supported programming languages to produce a high level skeleton of a file, with exact start-end lines of each item (e.g. a function's implementation is in lines 150-165). Encouraged to be used before reads. For my usage it adds 59 tok/turn but saves 224 tok/turn on read calls, saving 165 tok/turn.
* `code_execution` tool - uses [monty](https://github.com/pydantic/monty) to run an interpreter that has all other tools available as async functions. Maki uses it to filter / summarize / transform / pipe data to other tools as input, without it ever reaching and polluting the context window. Sandbox limited by time & memory.
* `task` tool - when delegating work to subagents, the AI chooses whether to run weak / medium / strong model of used provider. Think haiku / sonnet / opus.
* System prompt, tool descriptions, and tool examples are all concise, I've made sure not to bloat your context.
* Uses [rtk](https://github.com/rtk-ai/rtk) if you have it installed, disable with `--no-rtk`. Saves ~50% of bash output tokens. Remember bash is just 12% of total token usage, so 6% is nice, but saving on reads (65% of total) by using `index` gave me more benefit. I think I'll do bash output filtering like this myself in a future release.

User experience:
* SUPER fast startup, 60 FPS, and light on memory. Not running any javascript, using [ratatui](https://ratatui.rs) for TUI. Even the splash screen animation uses SIMD.
* Philosophy of not hiding anything - while other coding agents hide information as models improve (e.g. not showing number of lines read), maki leaves you in control.
* UI fits everything well on my small screen laptop.
* Full visibility of subagents - each subagent gets their own "chat window" you can easily navigate between using `/tasks` (Ctrl-X), or Ctrl-N/P.
* Sensible permission system - when the agent runs `git diff && rm -rf /`, what do you think will happen in your current coding agent? It will treat it as `git *`. Maki uses tree-sitter to parse the bash command and figure out the permissions requested are `git *` and `rm *`. Disable using `--yolo`.
* SSRF protection on `webfetch` calls.
* A `memory` tool to keep long term context, just tell maki to remember something (sometimes it uses it automatically). Managed via `/memory` (view / edit / delete memories).
* Fuzzy search with Ctrl-F.
* `/btw` to run a command with the chat history without interfering with the current session.
* Rewind on Escape-Escape (no code rewind yet, only chat history).
* Attach images in prompts.
* 26 of the most popular themes.
* Resume sessions.
* Skills & MCPs.
* Plan mode.
* Run bash commands using `!`, or `!!` if you want maki to not know about it.
* `/cd` to change dir.
* Use `--print --output-format stream-json` to run UI-less. Output is compatible with Claude Code, so you can easily replace your existing solutions (although I wouldn't recommend that, maki is very new).

Supported providers:
* Anthropic - `ANTHROPIC_API_KEY` only (using OAuth is against TOS).
* OpenAI - `OPENAI_API_KEY` and OAuth via `maki auth login openai`.
* Copilot - `GH_COPILOT_TOKEN` or an existing GitHub Copilot sign-in at `~/.config/github-copilot/`.
* Ollama - `OLLAMA_HOST` for local (e.g. `http://localhost:11434`), or `OLLAMA_API_KEY` for cloud.
* Mistral - `MISTRAL_API_KEY`.
* Z.AI - `ZHIPU_API_KEY`.
* Synthetic - `SYNTHETIC_API_KEY`.

**Dynamic providers** - drop an executable script into `~/.maki/providers/` to add custom providers or proxies. See [docs](https://maki.sh/docs/providers/#dynamic-providers) for details.

Recommended way to install:

```sh
# Download and read the script first (don't blindly trust shell scripts).
curl -fsSL https://maki.sh/install.sh -o install.sh
cat install.sh

# Then run.
chmod +x install.sh && sh install.sh
```

One-liner:

```sh
curl -fsSL https://maki.sh/install.sh | sh
```

Living on the edge (main branch):

```sh
cargo install --locked --git https://github.com/tontinton/maki.git maki
```

With Nix:

```sh
nix run github:tontinton/maki
```

Or download a pre-built binary from [GitHub Releases](https://github.com/tontinton/maki/releases/latest).

More info at the [official docs](http://maki.sh/docs).

---

## Fork changes (gburd/maki)

This fork tracks [tontinton/maki](https://github.com/tontinton/maki) upstream and adds the following features:

### Additional providers

* **AWS Bedrock** — native provider using AWS SigV4 signing. Set `AWS_REGION`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` (or use instance profiles). Supports all Bedrock-hosted Claude models.

### LSP integration

* 9 LSP tool operations (`goto_definition`, `find_references`, `hover`, `diagnostics`, etc.) via the `maki-lsp` crate, giving the agent IDE-level code intelligence.

### No phone-home

* The upstream version check on startup (`api.github.com/repos/.../releases/latest`) is now **opt-in**. Set `ui.check_for_updates = true` in config to re-enable it. Default is `false`.

### Sandbox mode

* `/sandbox` command toggles filesystem and network isolation for bash tool execution:
  * **Linux** — [Bubblewrap](https://github.com/containers/bubblewrap): read-only root bind, writable cwd + `/tmp`, no network.
  * **macOS** — Seatbelt (`sandbox-exec`): deny network and file-write except cwd + `/tmp`.

### Vi/Emacs keybindings

* `ui.keybindings` config option: `"default"`, `"emacs"`, or `"vi"`.
* Vi mode: Normal/Insert with mode indicator, operator-pending `d`/`y`, motions (`h/j/k/l`, `w/b/e`, `0/$`), actions (`i/a/A/I/o/O`, `x`, `dd/dw/d$`, `yy/yw/y$`, `p`).

### Static binary builds

* `[profile.release]` with LTO, single codegen unit, strip, `panic = "abort"`.
* Fully static Linux binaries via musl:
  ```sh
  rustup target add x86_64-unknown-linux-musl
  just static
  ```
* The Nix flake devShell includes `musl` for this purpose.

### Git state

```
upstream/main (007ba04) — Copilot provider, C++ indexer fix, lua async I/O
  └── 7e7c18a  feat: add native AWS Bedrock provider
  └── 6cb15c1  docs: regenerate providers page with Bedrock
  └── 0b51044  feat: add LSP integration with 9 tool operations
  └── 98a6496  docs: add LSP tools section
  └── d067f5c  refactor: rewrite Bedrock to use AWS crates
  └── ab2ce5b  fix: Bedrock model switching and endpoint URL encoding
  └── 6beffcf  feat: make update check opt-in (no-phone-home)
  └── 629b6a3  feat: add /sandbox toggle (sandbox)
  └── ba00a5d  feat: add vi/emacs keybindings (editor-keybindings)
  └── bfbefbd  release: bump to v0.2.9, static build support  ← v0.2.9 tag
```

---

> DISCLAIMER: >90% of code in maki was written by maki, guided by humans. The code is not as good as what I would've made in the artisinal hand-made style. But it's also not slop / vibe coded. I just think people should be honest about their use of AI in projects in this era.
