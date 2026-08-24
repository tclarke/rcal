import <nixpkgs> { overlays = [ rust-overlay ]; }

nixpkgs.mkShell {
  packages = with pkgs; [
    claude-code
    nodejs
    nix
    skills
    rtk
    ripgrep
    delta
    git
    git-lfs
    github-cli
  ];
}
