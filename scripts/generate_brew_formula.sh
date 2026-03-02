#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?usage: generate_brew_formula.sh <version> <base_url>}"
BASE_URL="${2:?usage: generate_brew_formula.sh <version> <base_url>}"
OUT_DIR="${3:-dist/release}"

archive="${OUT_DIR}/todero-native-darwin-aarch64-${VERSION}.tar.gz"
formula="${OUT_DIR}/todero-native.rb"

if [[ ! -f "${archive}" ]]; then
  echo "missing darwin archive for formula: ${archive}" >&2
  exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
  SHA="$(sha256sum "${archive}" | awk '{print $1}')"
else
  SHA="$(shasum -a 256 "${archive}" | awk '{print $1}')"
fi

URL="${BASE_URL%/}/native/todero-native-darwin-aarch64-${VERSION}.tar.gz"

cat > "${formula}" <<EOF
class ToderoNative < Formula
  desc "Todero Protocol V3 native runtime library"
  homepage "https://shellaia.com"
  version "${VERSION}"
  url "${URL}"
  sha256 "${SHA}"

  depends_on arch: :arm64

  def install
    native_dir = libexec/"native/darwin-aarch64"
    native_dir.mkpath
    cp_r Dir["darwin-aarch64/*"], native_dir
    ln_sf "darwin-aarch64", libexec/"native/current"

    (bin/"todero-native-info").write <<~EOS
      #!/usr/bin/env bash
      set -euo pipefail
      echo "#{libexec}/native/current"
    EOS
  end

  def caveats
    <<~EOS
      Set TODERO_V3_NATIVE_PATH to use this native runtime:
        export TODERO_V3_NATIVE_PATH="#{libexec}/native/current"
    EOS
  end

  test do
    assert_match "native/current", shell_output("#{bin}/todero-native-info")
  end
end
EOF

echo "created ${formula}"
