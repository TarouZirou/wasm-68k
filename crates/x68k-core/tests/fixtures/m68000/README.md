# SingleStepTests/m68000 fixtures

`representative.json` contains one case from each of ten instruction groups and
is executed by every `cargo test`. It was decoded from SingleStepTests/m68000
revision `64b253116a3de04aaac4346c43680960dc9b67e5` (MIT).

The 180 MiB full corpus is intentionally not vendored. The scheduled workflow
clones that exact revision and runs all verified groups. Upstream documents TAS
and TRAPV as unverified; they are therefore explicit expected failures rather
than silently accepted results.

To reproduce the full run locally:

```sh
git clone https://github.com/SingleStepTests/m68000 /tmp/m68000
git -C /tmp/m68000 checkout 64b253116a3de04aaac4346c43680960dc9b67e5
M68000_SST_DIR=/tmp/m68000/v1 \
M68000_SST_REVISION=64b253116a3de04aaac4346c43680960dc9b67e5 \
cargo test -p x68k-core --test singlestep_m68000 \
  full_binary_corpus_from_pinned_revision -- --ignored --exact
```
