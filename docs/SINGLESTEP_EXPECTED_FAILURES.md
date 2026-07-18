# SingleStepTests expected failures

Corpus revision: `64b253116a3de04aaac4346c43680960dc9b67e5`.

The scheduled full suite executes every v1 fixture except these two upstream
groups:

| Group | Status | Upstream reason |
| --- | --- | --- |
| TAS | expected failure | The fixture does not model the special five-cycle read-modify-write timing correctly. |
| TRAPV | expected failure | The fixture author reports unresolved triggering differences involving the S bit. |

Address-error (`re`/`we`) cases inside otherwise verified groups require pin-
accurate AS/UDS/LDS bus observation, which the dependency's `AddressBus` API
does not expose. The fixture runner skips those individual cases; dedicated
X68000 bus tests validate odd word/long accesses and exception-frame behavior.

An excluded group or case may only be enabled after its upstream issue is
resolved or a locally documented, independently verified expected result is
added.
