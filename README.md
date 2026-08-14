== Setting up claude
- install rtk
- install ripgrep
- Install caveman
  - `claude plugin marketplace add JuliusBrussee/caveman`
  - `claude plugin install caveman@caveman`
- Install PonyTail
  - `claude plugin markeyplace add DietrichGebert/ponytail`
  - `claude plugin install ponytail/ponytail`
- Optional: Install and configure OmniRoute
  - `npm install -g omniroute`
  - `omniroute server`
- Optional: Install skills
  - `npx skills add https://github.com/wshobson/agents --skill rust-async-patterns`

=== MCP Servers
- rust-mcp-server:
  - cargo install rust-mcp-server
  - cargo install cargo-machete
  - cargo install cargo-deny
  - `claude mcp add --scope user rust-mcp-server -- ~/.cargo/bin/rust-mcp-server`
- filesystem
  - `claude mcp add filesystem -s user  -- npx -y @modelcontextprotocol/server-filesystem ~/rcal`
- Optional: sequencial-thinking
  - `claude mcp add sequential-thinking -s user -- npx -y @modelcontextprotocol/server-sequential-thinking`
