# Literals: numbers, strings, paths, URIs, and the true/false/null identifiers.
{
  int = 42;
  float = 3.14;
  exponent = 1.5e3;
  string = "hello";
  escapes = "tab:\t newline:\n quote:\" dollar:\${not-interpolated}";
  multiline = ''
    first line
      indented line
    ''${not-interpolated}
  '';
  emptyString = "";
  absPath = /etc/hosts;
  relPath = ./01-literals.nix;
  homePath = ~/.config;
  uri = https://nixos.org;
  yes = true;
  no = false;
  nothing = null;
}
