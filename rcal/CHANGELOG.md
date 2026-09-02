# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
## [2.1.2] - 2026-09-02
### Bug Fixes

- Remove broken intra-doc link to CompressionExternalizer([`a53d764`](https://github.com/tclarke/rcal/commit/a53d7645ba59d0dcad8fa99ee7577249c55f59a8))


## [2.1.1] - 2026-09-02
### Bug Fixes

- Cleanup batch #62 #63 #64 #65 #74 #79([`ec4e8a6`](https://github.com/tclarke/rcal/commit/ec4e8a6ba93c5b51b5171ef7ef538589418bf044))
- Remove redundant ::default() call on unit struct XmlExternalizerLoader([`f2ab4a8`](https://github.com/tclarke/rcal/commit/f2ab4a8c414568f27981e0153d3722a0f08a5c81))


## [2.1.0] - 2026-08-24
### Bug Fixes

- Resolve P0 bugs in owner_producer mapping, service_status_loop macro, and DateTime overflow([`3aa7ac3`](https://github.com/tclarke/rcal/commit/3aa7ac36353a450d39c3b755eefc5ba26fa7f057))
- Resolve all P1 issues (safety, bug, docs)([`f639c6c`](https://github.com/tclarke/rcal/commit/f639c6c711f17592f87e899c45db8885b1802640))
- Lower test and example port numbers to 2000-range (review)([`7fa9bdb`](https://github.com/tclarke/rcal/commit/7fa9bdb4ed996a787018f698a1e3e840b8ea46ad))
- Implement AbstractService::reset() per CAL §5 lifecycle([`0384f01`](https://github.com/tclarke/rcal/commit/0384f01ad662a35a22197342de549175c2f5e19b))
### Documentation

- Add documentation to xsd-subset([`af4c24b`](https://github.com/tclarke/rcal/commit/af4c24b3f4214e3f2313a7a5583ddd662b87476b))
- Fix broken intra-doc links after CAL layer rename([`180d75e`](https://github.com/tclarke/rcal/commit/180d75e2a42944092cbe0c7338a621eb8a1427bc))
### Features

- **base**: Store and expose per-instance BoundedList bounds via const generics ([#52](https://github.com/tclarke/rcal/pull/52))([`6437b9c`](https://github.com/tclarke/rcal/commit/6437b9cc8009b687fa546f23eb3851d87c6ac18d))
- **asb**: Add UUID identity methods to AbstractServiceBus (issue #49)([`a3bf496`](https://github.com/tclarke/rcal/commit/a3bf496c3b644f1bfc893081de03767a68fe10da))
- Add xs::Base64Binary primitive type (closes #53)([`b96b31b`](https://github.com/tclarke/rcal/commit/b96b31bdf25cf48246192529a8e3a4be0731f5c7))
- Add AbstractServiceBus version and label methods (closes #51)([`24aba6b`](https://github.com/tclarke/rcal/commit/24aba6b42af877449a201ed78b65e3661c2f329e))
- Separate AbstractServiceBus from CAL layer (closes #72)([`978db85`](https://github.com/tclarke/rcal/commit/978db85216c23b6502167830f112335d1f619bf1))
- **service**: Add create_polling_reader returning Box<dyn AbstractReader> (closes #75)([`c0f1a34`](https://github.com/tclarke/rcal/commit/c0f1a34112288f404605c53bcc1bb1351b3ad423))
- **xs**: Add AnyUri, NormalizedString, Token type aliases (closes #77)([`46ca0db`](https://github.com/tclarke/rcal/commit/46ca0dbdf8c67bacc5a5d7f6c4ee089e6fee2ba9))
- **uci/base**: Add Externalizer/ExternalizerLoader abstraction (closes #54)([`23ff61f`](https://github.com/tclarke/rcal/commit/23ff61f6b0bc60435fb2a73a87407f026389c258))
- **externalizer**: Add ChainExternalizer, CompressionExternalizer, config-driven externalizer factory([`19ab467`](https://github.com/tclarke/rcal/commit/19ab46720d64dae34150980491a6cbfac2def6b5))
- **calconfig**: Add per-topic QoS config and apply reliability check (CAL-005210, CAL-005434)([`5476345`](https://github.com/tclarke/rcal/commit/54763455a5eaa2655ae0653ba6b6e06559ca0aa9))
- **codegen**: Add _set() and _disable() for optional fields (CAL-005290)([`66902d4`](https://github.com/tclarke/rcal/commit/66902d45d65cb2b33a2cfb9ee19962fa3eeb5921))
### Refactoring

- **asb**: Rename get_abstract_service_bus_connection_version to get_asb_connection_version([`a9b9887`](https://github.com/tclarke/rcal/commit/a9b9887ca235b5d7008e96c84360e551949b9d98))
- **externalizer**: Remove destroy_externalizer_loader; document drop([`25ad9dc`](https://github.com/tclarke/rcal/commit/25ad9dcc23153f4e1764076d75cfcaf83039d9b6))
- **externalizer**: Non-generic Externalizer trait, fluent builder, chain via next field([`a6aa419`](https://github.com/tclarke/rcal/commit/a6aa41902572955d53dc808f6faf26360182cc1e))

