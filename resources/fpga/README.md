# FPGA and firmware resources

These runtime resources are unmodified copies from the GPL-3.0-or-later DSView
reference checkout at `../DSView/DSView/res`, relative to this Rust DSView
repository root. The `.bin` files configure the matching instrument FPGA. The
legacy `.fw` files are retained for the FX2 models that use them in the original
application; firmware upload is not yet performed by this Rust transport. The
resources are not linked into the Rust binaries.

| File | Device | SHA-256 |
| --- | --- | --- |
| `DSCope.bin` | DSCope | `68f66919da02f518578cafc9f7c71f1eaf4a69cbaa9fa65e1f91998cfdd5c5f3` |
| `DSCope.fw` | DSCope FX2 firmware | `e5ff851442a78898273e9404de0c82983b241db9701277cc7777c03b13b4f668` |
| `DSCope20.bin` | DSCope20 / B20 | `a5e7bb1326fff891cd569daa0f9fd4d8f2241f735dcbd5bea0958a6adfd960d5` |
| `DSCope20.fw` | DSCope20 FX2 firmware | `5d4b250a0df596ed9e2fec5820541385e0bc5e241e806d7b218a037324994556` |
| `DSCopeC20B.bin` | DSCope C20 basic | `b1958e553822b400eb41638147269e0ac01f7d2ef3321115ef87e934d45ef263` |
| `DSCopeC20P.bin` | DSCope C20 / C20P | `646ba229683695f70980d222dd633fee1a2e17d1803a012cd172d455f3960341` |
| `DSCopeU2B100.bin` | DSCope U2B100 | `5220a2ccfd059d328447b2024fd28f31b1b3128abe27f30a4d4827fa55b62e76` |
| `DSCopeU2B20.bin` | DSCope U2B20 | `7ffa4649a11dfcf1d5cdabd52fdf7ce442a323d00b01a21ee126730286208550` |
| `DSCopeU2P20.bin` | DSCope U2P20 | `0934e074a0fa81e38f10db59bb8a2f77766c0c3b399ee444ccdc0b9c5bd86d58` |
| `DSCopeU3P100.bin` | DSCope U3P100 | `13fb46a1315b48948fac24df27e6de9330fa1add3182c41a3c1dccff49b4a76a` |
| `DSLogic.fw` | DSLogic FX2 firmware | `c55c71881e45f8087680268d00892ba3258740c019f2b1f1cf2eff06508dfa17` |
| `DSLogic33.bin` | DSLogic 3.3 V threshold | `f8980ad20504e8bae3af1ef9d7fb92ab9f8df05b4ae6d124da9f7eb6cab84df5` |
| `DSLogic50.bin` | DSLogic 5.0 V threshold | `2c7dc21a74adf60b7dbf357f237d613bbddfff75e21f0b9a0474874f4ba58463` |
| `DSLogicBasic.bin` | DSLogic Basic | `4496c81a6211e41550246213e594c3f64f416e9e7d594db64efc69e53b54fdd7` |
| `DSLogicPlus-pgl12-2.bin` | DSLogic Plus PGL12 rev. 2 | `b4ee3d3042d8c7cf3536403407490ffabc7ff0699332da331a04c1e6f0bad067` |
| `DSLogicPlus-pgl12.bin` | DSLogic Plus PGL12 | `340413e053a765c38ee87e89f45a55ad2530a98ad132ccb7e8a9a0b0c99d0ee2` |
| `DSLogicPlus.bin` | DSLogic Plus | `6a6f8dc0ed27dbfbd41dd98b030da52d8e2c5c7f840c23ed903c7d5fe38e11dd` |
| `DSLogicPro.bin` | DSLogic Pro | `69a33af226b627cc56bf38d17538fdef0118f86ee6381254a4c1c5bac8cbf949` |
| `DSLogicPro.fw` | DSLogic Pro FX2 firmware | `6385f20bccdc92c02ec9571dcf1c18f08f08089b4f16817bb287c1e29c910742` |
| `DSLogicU2Basic-pgl12-2.bin` | DSLogic U2Basic PGL12 rev. 2 | `1ec5e3016a80e7ddad90afdace52358ca0a4e38df4e22f761a9c2afd542db9d5` |
| `DSLogicU2Basic-pgl12.bin` | DSLogic U2Basic PGL12 | `997acd8adeeb7a28eb40dc3e53ea33569725b88c85bb4f2dd711e37f86ea97d3` |
| `DSLogicU2Basic.bin` | DSLogic U2Basic | `3172dcbc91bcaf76faf3f99e56c113fafcc55ed960252cd85b5e65d3e6caba81` |
| `DSLogicU2Pro16.bin` | DSLogic U2Pro16 | `c0be3b52222a9efea8fc5cc59f579c6880e73e9f103982d2aeb4c443ca64d8c9` |
| `DSLogicU3Pro16.bin` | DSLogic U3Pro16 | `0ae5a6e52eb9d6db27c3591d3d0df5c34c48c8509d25c65c6ea1f008f622a56e` |
| `DSLogicU3Pro32.bin` | DSLogic U3Pro32 | `0dc3251be0476cf964fa521ce2a34651ae82724e96fd4272cadc0cc27c86a17d` |

The upstream DSView project identifies the program as GPL version 3 or later.
See the repository-level licensing information and the upstream `COPYING` file
for the applicable terms.
