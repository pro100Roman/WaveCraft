# FPGA resources

These FPGA bitstreams are unmodified copies from the GPL-3.0-or-later DSView
reference checkout at `../DSView/DSView/res`, relative to this Rust DSView
repository root. DSView loads them at runtime when preparing the matching
instrument; they are not linked into the Rust binaries.

| File | Device | SHA-256 |
| --- | --- | --- |
| `DSLogicU3Pro16.bin` | DSLogic U3Pro16 | `0ae5a6e52eb9d6db27c3591d3d0df5c34c48c8509d25c65c6ea1f008f622a56e` |
| `DSCopeU3P100.bin` | DSCope U3P100 | `13fb46a1315b48948fac24df27e6de9330fa1add3182c41a3c1dccff49b4a76a` |

The upstream DSView project identifies the program as GPL version 3 or later.
See the repository-level licensing information and the upstream `COPYING` file
for the applicable terms.
