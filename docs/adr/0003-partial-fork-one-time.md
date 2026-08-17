# Partial fork of waku — one-time inheritance

daku bootstraps by **forking only what v1 needs** from [egoist/waku](https://github.com/egoist/waku): the GPUI client (`src/`), Rust daemon / protocol / `waku-core`, and the SQLite migration pipeline — then stripping the agent domain and re-typing protocol payloads for Signals and Environments. It is a **one-time inheritance**, not an upstream we keep merging; waku remains a reference.

Keep the **daemon + versioned protocol + native client** split (local in v1; hostable later).

## Considered options

- **Fork the whole monorepo and delete** — carries `apps/web`, website, and unused crates for no gain.
- **Clean repo, port by hand** — redoes GPUI scaffolding against the egoist/zed pin; slower path to the same look.
- **Track upstream long-term** — rejected; waku evolves as an agent product, not a monitoring shell.
