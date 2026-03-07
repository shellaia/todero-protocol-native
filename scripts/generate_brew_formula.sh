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

URL="${BASE_URL%/}/todero-native-darwin-aarch64-${VERSION}.tar.gz"

cat > "${formula}" <<EOF
class ToderoNative < Formula
  desc "Todero Protocol V3 native runtime library"
  homepage "https://shellaia.com"
  version "${VERSION}"
  url "${URL}"
  sha256 "${SHA}"

  depends_on arch: :arm64

  def install
    source_dir = buildpath
    odie "missing native payload file: #{source_dir}/libv3_ffi.dylib" unless (source_dir/"libv3_ffi.dylib").exist?
    odie "missing native payload file: #{source_dir}/metadata.json" unless (source_dir/"metadata.json").exist?

    native_dir = libexec/"native/darwin-aarch64"
    native_dir.mkpath
    native_dir.install source_dir.children
    ln_sf "darwin-aarch64", libexec/"native/current"

    (bin/"tninfo").write <<~EOS
      #!/usr/bin/env bash
      set -euo pipefail
      if [[ "\${1:-}" != "--libdir" ]]; then
        echo "usage: tninfo --libdir" >&2
        exit 2
      fi
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
    assert_match "native/current", shell_output("#{bin}/tninfo --libdir")
  end
end
EOF

echo "created ${formula}"
