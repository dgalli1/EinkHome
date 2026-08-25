#!/bin/sh
#
# make-apk.sh — build the Android APK for EinkHome.
#
# One APK carries both ABIs (arm64-v8a for devices, x86_64 for
# emulators); NativeActivity picks the right .so at install time.
# Packaging is hand-rolled (no gradle): aapt2 link the manifest, append
# the native libs, zipalign, sign with a generated debug keystore.
#
# Outputs:
#   build/einkhome-dev.apk
#   build/debug.keystore            (generated on first run)
#
# Environment (all optional — resolved from ANDROID_HOME when unset):
#   ANDROID_HOME   Android SDK root (ndk/, build-tools/, platforms/)
#   ANDROID_NDK    NDK root          (default: newest under $ANDROID_HOME/ndk)
#   APK_BUILD_TOOLS  build-tools dir (default: newest under $ANDROID_HOME/build-tools)
#   APK_PLATFORM_JAR platform jar    (default: $ANDROID_HOME/platforms/android-34/android.jar)
#
set -eu

HERE=$(
	unset CDPATH
	cd "$(dirname "$0")" && pwd
)
REPO_ROOT=$(
	unset CDPATH
	cd "${HERE}/.." && pwd
)
EH_UI="${REPO_ROOT}/eh_ui"
OUT_DIR="${REPO_ROOT}/build"
APK="${OUT_DIR}/einkhome-dev.apk"

ANDROID_HOME=${ANDROID_HOME:-/opt/android-sdk}
[ -d "${ANDROID_HOME}" ] || ANDROID_HOME="${HOME}/Android/Sdk"

# NDK: newest installed, unless ANDROID_NDK says otherwise.
if [ -z "${ANDROID_NDK:-}" ]; then
	ANDROID_NDK=$(ls -d "${ANDROID_HOME}"/ndk/* 2>/dev/null | sort -V | tail -1)
fi
[ -n "${ANDROID_NDK}" ] && [ -d "${ANDROID_NDK}" ] || {
	echo "ERROR: no NDK under ${ANDROID_HOME}/ndk (set ANDROID_NDK)" >&2
	exit 1
}

# build-tools: newest installed, unless APK_BUILD_TOOLS says otherwise.
if [ -z "${APK_BUILD_TOOLS:-}" ]; then
	APK_BUILD_TOOLS=$(ls -d "${ANDROID_HOME}"/build-tools/* 2>/dev/null | sort -V | tail -1)
fi
[ -n "${APK_BUILD_TOOLS}" ] && [ -d "${APK_BUILD_TOOLS}" ] || {
	echo "ERROR: no build-tools under ${ANDROID_HOME}/build-tools" >&2
	exit 1
}

APK_PLATFORM_JAR=${APK_PLATFORM_JAR:-"${ANDROID_HOME}/platforms/android-34/android.jar"}
[ -f "${APK_PLATFORM_JAR}" ] || {
	echo "ERROR: platform jar missing: ${APK_PLATFORM_JAR}" >&2
	exit 1
}

TOOLCHAIN="${ANDROID_NDK}/toolchains/llvm/prebuilt/linux-x86_64/bin"
MANIFEST="${EH_UI}/crates/eh_android/AndroidManifest.xml"
AAPT2="${APK_BUILD_TOOLS}/aapt2"
ZIPALIGN="${APK_BUILD_TOOLS}/zipalign"
APKSIGNER="${APK_BUILD_TOOLS}/apksigner"

echo "==> ndk        ${ANDROID_NDK}"
echo "==> build-tools ${APK_BUILD_TOOLS}"

build_abi() {
	_triple=$1
	_abi=$2
	_uc=$(echo "${_triple}" | tr 'a-z-' 'A-Z_')
	echo "==> cargo eh_android for ${_triple} (${_abi})"
	# The triple's hyphens are illegal in shell identifiers, so the
	# cc-crate variables go through env(1); the linker var uses the
	# uppercase-underscore form cargo accepts.
	env \
		"CC_${_triple}=${TOOLCHAIN}/${_triple}34-clang" \
		"AR_${_triple}=${TOOLCHAIN}/llvm-ar" \
		"CARGO_TARGET_${_uc}_LINKER=${TOOLCHAIN}/${_triple}34-clang" \
		cargo build --release -p eh_android --target "${_triple}" \
		--manifest-path "${EH_UI}/Cargo.toml"
	mkdir -p "${OUT_DIR}/apk-libs/lib/${_abi}"
	cp "${EH_UI}/target/${_triple}/release/libeh_android.so" \
		"${OUT_DIR}/apk-libs/lib/${_abi}/libeh_android.so"
}

build_abi aarch64-linux-android arm64-v8a
build_abi x86_64-linux-android x86_64

# Debug keystore (generated once, reused so installs update in place).
KEYSTORE="${OUT_DIR}/debug.keystore"
if [ ! -f "${KEYSTORE}" ]; then
	echo "==> generating debug keystore"
	keytool -genkeypair -keystore "${KEYSTORE}" -storepass android \
		-keypass android -alias androiddebugkey -dname "CN=EinkHome Dev" \
		-keyalg RSA -keysize 2048 -validity 10000 2>/dev/null
fi

echo "==> packaging ${APK}"
STAGE="${OUT_DIR}/apk-libs"
rm -f "${STAGE}/base.apk" "${STAGE}/aligned.apk" "${APK}"
"${AAPT2}" link --manifest "${MANIFEST}" -I "${APK_PLATFORM_JAR}" \
	-o "${STAGE}/base.apk"
(
	cd "${STAGE}" &&
		if command -v zip >/dev/null 2>&1; then
			zip -q base.apk lib/arm64-v8a/libeh_android.so lib/x86_64/libeh_android.so
		else
			# No zip(1): aapt2's output is already a zip; python is
			# always present on dev machines and CI runners.
			python3 -c "
import zipfile
with zipfile.ZipFile('base.apk', 'a', zipfile.ZIP_DEFLATED) as z:
    z.write('lib/arm64-v8a/libeh_android.so', 'lib/arm64-v8a/libeh_android.so')
    z.write('lib/x86_64/libeh_android.so', 'lib/x86_64/libeh_android.so')
"
		fi
)
"${ZIPALIGN}" -f 4 "${STAGE}/base.apk" "${STAGE}/aligned.apk"
"${APKSIGNER}" sign --ks "${KEYSTORE}" --ks-pass pass:android \
	--out "${APK}" "${STAGE}/aligned.apk"

rm -rf "${STAGE}"
echo "==> ${APK}"
