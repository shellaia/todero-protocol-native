#!/usr/bin/env bash
set -euo pipefail

BEGIN_MARKER="# >>> todero-native >>>"
END_MARKER="# <<< todero-native <<<"

usage() {
  cat <<'EOF'
usage: profile_env_setup.sh [--apply|--print-cleanup]
EOF
}

target_user() {
  if [[ -n "${TODERO_PROFILE_TARGET_USER:-}" ]]; then
    printf '%s\n' "${TODERO_PROFILE_TARGET_USER}"
    return 0
  fi
  if [[ -n "${SUDO_USER:-}" && "${SUDO_USER}" != "root" ]]; then
    printf '%s\n' "${SUDO_USER}"
    return 0
  fi
  id -un
}

target_home() {
  local user="$1"
  if [[ -n "${TODERO_PROFILE_TARGET_HOME:-}" ]]; then
    printf '%s\n' "${TODERO_PROFILE_TARGET_HOME}"
    return 0
  fi
  if command -v getent >/dev/null 2>&1; then
    local h
    h="$(getent passwd "${user}" | awk -F: '{print $6}')"
    if [[ -n "${h}" ]]; then
      printf '%s\n' "${h}"
      return 0
    fi
  fi
  if command -v dscl >/dev/null 2>&1; then
    local h
    h="$(dscl . -read "/Users/${user}" NFSHomeDirectory 2>/dev/null | awk '{print $2}')"
    if [[ -n "${h}" ]]; then
      printf '%s\n' "${h}"
      return 0
    fi
  fi
  if [[ -n "${HOME:-}" ]]; then
    printf '%s\n' "${HOME}"
  fi
}

strip_managed_block() {
  local path="$1"
  local tmp="$2"
  awk -v begin="${BEGIN_MARKER}" -v end="${END_MARKER}" '
    $0 == begin { in_block = 1; next }
    $0 == end { in_block = 0; next }
    in_block == 0 { print }
  ' "${path}" > "${tmp}"
}

upsert_file_block() {
  local file_path="$1"
  local body="$2"
  local dir_path
  local tmp_file

  dir_path="$(dirname "${file_path}")"
  mkdir -p "${dir_path}"
  touch "${file_path}"

  tmp_file="$(mktemp)"
  strip_managed_block "${file_path}" "${tmp_file}"

  {
    cat "${tmp_file}"
    if [[ -s "${tmp_file}" ]]; then
      printf '\n'
    fi
    printf '%s\n' "${BEGIN_MARKER}"
    printf '%s\n' "${body}"
    printf '%s\n' "${END_MARKER}"
  } > "${file_path}"
  rm -f "${tmp_file}"
}

print_cleanup() {
  local home_dir="$1"
  cat <<EOF
todero-native uninstall note:
  startup snippet was not removed automatically.
  remove it manually from:
    ${home_dir}/.bashrc
    ${home_dir}/.zshrc
    ${home_dir}/.config/fish/conf.d/todero-native.fish
  snippet markers:
    ${BEGIN_MARKER}
    ${END_MARKER}
EOF
}

apply_profile() {
  local user
  local home
  local bash_block
  local fish_block

  user="$(target_user)"
  home="$(target_home "${user}")"
  if [[ -z "${home}" || ! -d "${home}" ]]; then
    echo "todero-native: unable to resolve target home for user=${user}" >&2
    echo 'todero-native: set manually: export TODERO_V3_NATIVE_PATH="$(tninfo --libdir)"' >&2
    return 1
  fi

  bash_block='if command -v tninfo >/dev/null 2>&1; then
  export TODERO_V3_NATIVE_PATH="$(tninfo --libdir)"
fi'
  fish_block='if type -q tninfo
    set -gx TODERO_V3_NATIVE_PATH (tninfo --libdir)
end'

  upsert_file_block "${home}/.bashrc" "${bash_block}"
  upsert_file_block "${home}/.zshrc" "${bash_block}"
  upsert_file_block "${home}/.config/fish/conf.d/todero-native.fish" "${fish_block}"

  cat <<EOF
todero-native: configured TODERO_V3_NATIVE_PATH startup snippets for user=${user}.
todero-native: open a new shell session to apply environment changes.
EOF
}

main() {
  local mode="${1:---apply}"
  case "${mode}" in
    --apply)
      apply_profile
      ;;
    --print-cleanup)
      local user
      local home
      user="$(target_user)"
      home="$(target_home "${user}")"
      if [[ -z "${home}" ]]; then
        home="~"
      fi
      print_cleanup "${home}"
      ;;
    -h|--help)
      usage
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
}

main "${1:-}"
