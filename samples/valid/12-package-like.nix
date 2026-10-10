# A realistic nixpkgs-style package function. Evaluating this file returns a
# function; call it with stub arguments to evaluate the body without nixpkgs:
#   nix eval --expr 'import ./12-package-like.nix {
#     lib = { licenses.mit = "MIT"; optional = c: x: if c then [ x ] else [ ]; };
#     stdenv = { mkDerivation = a: a; }; fetchurl = a: a.url; openssl = "openssl"; }'
{ lib
, stdenv
, fetchurl
, openssl
, withSsl ? true
, ...
}:

let
  pname = "hello";
  version = "2.12.1";
in
stdenv.mkDerivation rec {
  inherit pname version;

  src = fetchurl {
    url = "mirror://gnu/${pname}/${pname}-${version}.tar.gz";
    sha256 = "0000000000000000000000000000000000000000000000000000";
  };

  buildInputs = lib.optional withSsl openssl;

  configureFlags = [ "--prefix=$out" ]
    ++ lib.optional withSsl "--with-ssl";

  meta = with lib; {
    description = "A program that produces a familiar, friendly greeting";
    license = licenses.mit;
    mainProgram = pname;
  };
}
