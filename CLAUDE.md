# Description
rcal is an Open Mission Systems (OMS) Critical Abstraction Layer (CAL) implementation for Rust. It shall implement the "CERT CAL-" requirements and take inspiration from the "CERT CXX-" requirements. When naming functions, methods, structs, etc. follow the C++ CAL requirements. When deciding on capabilities or implementation, follow the C++ CAL requirements when it makes sense put prioritize good Rust practices. Use Rust features when they will benefit the interface. You should target Rust 2020 unless there is a compelling reason to use Rust 2024. Never use experimental features not available in the Rust stable toolchain.

# Directory layout
 - The `design` directory contains design notes and the archimate design document.
 - The `rcal` directory contains the Rust library for rcal.
 - The `specs` directory contains documents which define the CAL requirements and design.

## Specifications and Documentation Resources
The `04_OMSC-SPC-001_RevL_CAL_Specification_DandD_v2_5.docx.pdf` file contains all the the general CAL requirements and description.

The `06_OMSC-SPC-008_RevK_CxxCALSpec_DandD_v2_5.docx.pdf` contains all of the C++ requirements and description.

When asked to evaluate specific requirements, use the `specs/cal_cert_requirements.md` file which summarizes all of the CERT CAL requirements. Only read .pdf specification files if the information is not available in the requirements table.

When asked about UCI schema types, elements, or message definitions, use `specs/schema_summary.md` first. Only read the raw `.xsd` files if the information is not available in the summary.

## Design
The `oms_rust_cal_core.archimate` file is the defacto design document and should always be maintained to match the Rust implementation. Before making large changes to code, always change the archimate design first and only when accepted should you create Rust code.

The `Design Rust CAL interfaces (exceptions, UUIDs, bus).txt` file is a saved conversation with Claude Sonnet 4.6 and represents the earliest part of the design process. Nothing in there should be assumed to be the real design unless it corresponds with the archimate and Rust code files. It may be used as clarification and additional information if needed.

`archimate_layout.py` is a tool to create a visually pleasing archimate View by using force directed layout to position items in the view.

All `.bak` files should be ignored and are simply backup files saved by the Arch tool. Never look at these files unless they are explicitly added to a conversation.

## Rust code directory
This is a standard cargo layout. all `.rs` and `.toml` files can be used as input. Ignore the `target` directory and other intermediate files.

# Coding standards
 - Follow the defacto rust code formatting and design standards
 - Avoid adding new dependencies unless needed. Always ask before adding a dependency.
 - New features of existing dependencies may be added without asking.
 - Always create unit tests in the `test` private model when a test can be written.
 - Purely abstract interfaces do not need unit tests unless you are explicitly directed to add them.
 - Always use modules based on the C++ namespaces in the "CERT C++"
 - Additional modules may be added for Rust specific tools, interfaces, and implementations. For example, the abstract service bus (ASB) interfaces will be in the `uci::base` module but ASB implementations can be in `uci::asb` or `asb` modules.
