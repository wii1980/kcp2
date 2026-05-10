#!/usr/bin/env bash
# =============================================================================
# kcp2 QA 脚本
# 执行: cargo check, clippy, test, build --examples, bench check
# 自动切换工具链: 1.75 (core/std) / 1.85+ (embassy)
# 用法: ./qa.sh [--no-clippy] [--no-bench]
# =============================================================================

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

PASS=0
FAIL=0
WARN=0
SKIP=0

# ---------------------------------------------------------------------------
# 参数解析
# ---------------------------------------------------------------------------
RUN_CLIPPY=true
RUN_BENCH=true

for arg in "$@"; do
	case "$arg" in
	--no-clippy) RUN_CLIPPY=false ;;
	--no-bench) RUN_BENCH=false ;;
	--help | -h)
		echo "用法: $0 [--no-clippy] [--no-bench]"
		echo ""
		echo "选项:"
		echo "  --no-clippy   跳过 clippy 检查"
		echo "  --no-bench    跳过 benchmark 编译检查"
		exit 0
		;;
	*)
		echo -e "${RED}未知参数: $arg${NC}"
		exit 1
		;;
	esac
done

# ---------------------------------------------------------------------------
# 工具函数
# ---------------------------------------------------------------------------
step_header() {
	echo ""
	echo -e "${CYAN}========================================${NC}"
	echo -e "${CYAN}  $1${NC}"
	echo -e "${CYAN}========================================${NC}"
}

pass() {
	echo -e "  ${GREEN}✓ PASS${NC}: $1"
	((PASS++)) || true
}

fail() {
	echo -e "  ${RED}✗ FAIL${NC}: $1"
	((FAIL++)) || true
}

warn() {
	echo -e "  ${YELLOW}⚠ WARN${NC}: $1"
	((WARN++)) || true
}

skip() {
	echo -e "  ${CYAN}⊘ SKIP${NC}: $1"
	((SKIP++)) || true
}

# ---------------------------------------------------------------------------
# 工具链检测
# ---------------------------------------------------------------------------
ORIGINAL_TOOLCHAIN=$(rustup default 2>/dev/null | sed 's/ .*//' || echo "1.75")
STD_TOOLCHAIN=""
for tc in 1.75 1.75.0; do
	if rustup toolchain list 2>/dev/null | grep -q "$tc-x86_64-unknown-linux-gnu"; then
		STD_TOOLCHAIN="$tc"
		break
	fi
done
[ -z "$STD_TOOLCHAIN" ] && STD_TOOLCHAIN="$ORIGINAL_TOOLCHAIN"

EMBASSY_TOOLCHAIN=""
for tc in 1.92 1.92.0 1.91.1 1.88 1.87 1.86 1.85 stable nightly; do
	if rustup toolchain list 2>/dev/null | grep -q "$tc-x86_64-unknown-linux-gnu"; then
		EMBASSY_TOOLCHAIN="$tc"
		break
	fi
done

restore_toolchain() {
	if [ -n "${ORIGINAL_TOOLCHAIN:-}" ]; then
		rustup default "$ORIGINAL_TOOLCHAIN" 2>/dev/null || true
	fi
}
trap restore_toolchain EXIT

step_header "0/7 工具链检测"
echo -e "  原始默认:  ${CYAN}$ORIGINAL_TOOLCHAIN${NC}"
echo -e "  std 检查:  ${CYAN}$STD_TOOLCHAIN${NC}  (kcp2-core, kcp2-std, kcp2)"
echo -e "  embassy:   ${CYAN}${EMBASSY_TOOLCHAIN:-未安装}${NC}  (kcp2-embassy, 需要 1.85+)"

rustup default "$STD_TOOLCHAIN" 2>/dev/null || true
echo -e "  已切换到:  ${GREEN}$(rustc --version)${NC}"

# ---------------------------------------------------------------------------
# 1. cargo check — kcp2-core / kcp2-std / kcp2 (Rust 1.75)
# ---------------------------------------------------------------------------
step_header "1/7 cargo check [Rust $STD_TOOLCHAIN]"

check_features() {
	local label="$1"
	shift
	if cargo check --all-targets "$@" 2>&1; then
		pass "$label"
	else
		fail "$label"
	fi
}

check_features "kcp2-core (default features)" -p kcp2-core
check_features "kcp2-core (alloc only)" -p kcp2-core --no-default-features --features alloc
check_features "kcp2-core (heapless only)" -p kcp2-core --no-default-features --features heapless
check_features "kcp2-core (alloc + bytes)" -p kcp2-core --features bytes
check_features "kcp2-std" -p kcp2-std
check_features "kcp2-std (aead)" -p kcp2-std --features aead
check_features "kcp2-std (dtls)" -p kcp2-std --features dtls
check_features "kcp2 (compat layer)" -p kcp2
check_features "workspace (default)" --workspace --exclude kcp2-embassy

# ---------------------------------------------------------------------------
# 1b. cargo check — kcp2-embassy (Rust 1.85+)
# ---------------------------------------------------------------------------
step_header "1b/7 cargo check [Rust ${EMBASSY_TOOLCHAIN:-N/A}] kcp2-embassy"

if [ -n "$EMBASSY_TOOLCHAIN" ]; then
	rustup default "$EMBASSY_TOOLCHAIN" 2>/dev/null || true
	echo "  $(rustc --version)"
	if cargo check -p kcp2-embassy --all-targets 2>&1; then
		pass "kcp2-embassy (Rust $EMBASSY_TOOLCHAIN)"
	else
		fail "kcp2-embassy (Rust $EMBASSY_TOOLCHAIN)"
	fi
	rustup default "$STD_TOOLCHAIN" 2>/dev/null || true
else
	skip "kcp2-embassy (无可用 Rust 1.85+ 工具链)"
fi

# ---------------------------------------------------------------------------
# 2. cargo clippy [Rust $STD_TOOLCHAIN]
# ---------------------------------------------------------------------------
step_header "2/7 cargo clippy [Rust $STD_TOOLCHAIN]"

if $RUN_CLIPPY; then
	if cargo clippy --workspace --exclude kcp2-embassy --all-targets -- \
		-D clippy::all -A clippy::pedantic 2>&1; then
		pass "clippy (workspace)"
	else
		fail "clippy 检查失败"
	fi
else
	skip "clippy (已禁用)"
fi

# ---------------------------------------------------------------------------
# 3. cargo test [Rust $STD_TOOLCHAIN]
# ---------------------------------------------------------------------------
step_header "3/7 cargo test [Rust $STD_TOOLCHAIN]"

if cargo test --workspace --exclude kcp2-embassy 2>&1; then
	pass "workspace tests (default)"
else
	fail "workspace tests (default)"
fi

if cargo test --workspace --exclude kcp2-embassy --features aead 2>&1; then
	pass "workspace tests (aead)"
else
	fail "workspace tests (aead)"
fi

if cargo test --workspace --exclude kcp2-embassy --features dtls 2>&1; then
	pass "workspace tests (dtls)"
else
	fail "workspace tests (dtls)"
fi

# ---------------------------------------------------------------------------
# 4. cargo test --doc [Rust $STD_TOOLCHAIN]
# ---------------------------------------------------------------------------
step_header "4/7 cargo test --doc [Rust $STD_TOOLCHAIN]"

if cargo test --workspace --exclude kcp2-embassy --doc 2>&1; then
	pass "doc tests (default)"
else
	fail "doc tests (default)"
fi

if cargo test --workspace --exclude kcp2-embassy --features aead --doc 2>&1; then
	pass "doc tests (aead)"
else
	fail "doc tests (aead)"
fi

if cargo test --workspace --exclude kcp2-embassy --features dtls --doc 2>&1; then
	pass "doc tests (dtls)"
else
	fail "doc tests (dtls)"
fi

# ---------------------------------------------------------------------------
# 5. cargo build — examples [Rust $STD_TOOLCHAIN + esp]
# ---------------------------------------------------------------------------
step_header "5/7 cargo build --examples"

EXAMPLES=(
	"echo"
	"high_level_api"
	"heartbeat"
	"multi_server"
	"udp_echo"
	"performance_test"
	"dtls_echo"
	"aead_echo"
)

for ex in "${EXAMPLES[@]}"; do
	if cargo build --example "$ex" 2>&1; then
		pass "example: $ex"
	else
		fail "example: $ex"
	fi
done

# dtls_echo 需启用 dtls feature 才能真正连通
if cargo build --example dtls_echo --features dtls 2>&1; then
	pass "example: dtls_echo (--features dtls)"
else
	fail "example: dtls_echo (--features dtls)"
fi

# aead_echo 需启用 aead feature
if cargo build --example aead_echo --features aead 2>&1; then
	pass "example: aead_echo (--features aead)"
else
	fail "example: aead_echo (--features aead)"
fi

# embassy-esp32 (合并 example，通过 feature 选择芯片)
ESP_TOOLCHAIN_DIR="$HOME/.rustup/toolchains/esp"

if rustup target list --installed 2>/dev/null | grep -q 'riscv32imc-unknown-none-elf'; then
	if cargo build --manifest-path examples/embassy-esp32/Cargo.toml --target riscv32imc-unknown-none-elf --features esp32c3 --release 2>&1; then
		pass "example: embassy-esp32 (esp32c3)"
	else
		fail "example: embassy-esp32 (esp32c3)"
	fi
else
	skip "example: embassy-esp32 (esp32c3, 需要 riscv32imc target)"
fi

ESP_CARGO="$ESP_TOOLCHAIN_DIR/bin/cargo"
ESP_RUSTC="$ESP_TOOLCHAIN_DIR/bin/rustc"
if [ -x "$ESP_CARGO" ] && "$ESP_RUSTC" --print target-list 2>/dev/null | grep -q 'xtensa-esp32s3-none-elf'; then
	XTENSA_GCC_BASE=$(find "$ESP_TOOLCHAIN_DIR/xtensa-esp-elf" -maxdepth 4 -name "xtensa-esp32s3-elf-gcc" -print -quit 2>/dev/null)
	if [ -n "$XTENSA_GCC_BASE" ]; then
		export PATH="$(dirname "$XTENSA_GCC_BASE"):$PATH"
	fi
	export LIBCLANG_PATH="$ESP_TOOLCHAIN_DIR/xtensa-esp32-elf-clang/esp-20.1.1_20250829/esp-clang/lib"
	export RUSTUP_TOOLCHAIN=esp

	if "$ESP_CARGO" build --manifest-path examples/embassy-esp32/Cargo.toml \
		--target xtensa-esp32s3-none-elf --features esp32s3 \
		-Z build-std=core,alloc --release 2>&1; then
		pass "example: embassy-esp32 (esp32s3)"
	else
		fail "example: embassy-esp32 (esp32s3)"
	fi
else
	skip "example: embassy-esp32 (esp32s3, 需要 esp 工具链)"
fi

# ---------------------------------------------------------------------------
# 6. cargo bench — 编译检查 + Ignored 基准 [Rust $STD_TOOLCHAIN]
# ---------------------------------------------------------------------------
step_header "6/7 cargo bench [Rust $STD_TOOLCHAIN]"

if $RUN_BENCH; then
	if cargo bench --no-run 2>&1; then
		pass "benchmarks compile"
	else
		fail "benchmarks compile"
	fi

	if cargo bench -- --ignored 2>&1; then
		pass "benchmarks run (--ignored)"
	else
		# 基准测试可能因时间过长而超时，不作为硬性失败
		warn "benchmarks run (--ignored) 未完全通过（可手动重试：cargo bench -- --ignored）"
	fi
else
	skip "benchmarks (已禁用)"
fi

# ---------------------------------------------------------------------------
# 7. no_std 兼容性检查
# ---------------------------------------------------------------------------
step_header "7/7 no_std 兼容性检查"

if cargo check -p kcp2-core --no-default-features --features alloc 2>&1; then
	pass "kcp2-core (no_std + alloc)"
else
	fail "kcp2-core (no_std + alloc)"
fi

if cargo check -p kcp2-core --no-default-features --features heapless 2>&1; then
	pass "kcp2-core (no_std + heapless)"
else
	fail "kcp2-core (no_std + heapless)"
fi

if [ -n "$EMBASSY_TOOLCHAIN" ]; then
	rustup default "$EMBASSY_TOOLCHAIN" 2>/dev/null || true
	echo "  $(rustc --version)"
	if cargo check -p kcp2-embassy 2>&1; then
		pass "kcp2-embassy (no_std, Rust $EMBASSY_TOOLCHAIN)"
	else
		fail "kcp2-embassy (no_std, Rust $EMBASSY_TOOLCHAIN)"
	fi
	rustup default "$STD_TOOLCHAIN" 2>/dev/null || true
else
	skip "kcp2-embassy (no_std, 无可用 Rust 1.85+ 工具链)"
fi

# ---------------------------------------------------------------------------
# 汇总
# ---------------------------------------------------------------------------
step_header "QA 结果汇总"

TOTAL=$((PASS + FAIL + WARN + SKIP))
echo -e "  ${GREEN}PASS${NC}: $PASS"
echo -e "  ${RED}FAIL${NC}: $FAIL"
echo -e "  ${YELLOW}WARN${NC}: $WARN"
echo -e "  ${CYAN}SKIP${NC}: $SKIP"
echo -e "  总计:  $TOTAL"
echo ""

if [ "$FAIL" -gt 0 ]; then
	echo -e "${RED}✗ QA 未通过: $FAIL 项失败${NC}"
	exit 1
else
	echo -e "${GREEN}✓ QA 全部通过${NC}"
	exit 0
fi
