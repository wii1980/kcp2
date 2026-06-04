#!/usr/bin/env bash
# =============================================================================
# kcp2 crates.io 发布脚本
# 先检查（check + test + clippy），通过后按依赖顺序发布
# 用法: ./publish.sh [--dry-run] [--skip-check] [--skip-embassy]
# =============================================================================

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

DRY_RUN=false
SKIP_CHECK=false
SKIP_EMBASSY=false
REGISTRY="--registry crates-io"

for arg in "$@"; do
	case "$arg" in
	--dry-run) DRY_RUN=true ;;
	--skip-check) SKIP_CHECK=true ;;
	--skip-embassy) SKIP_EMBASSY=true ;;
	--help | -h)
		echo "用法: $0 [--dry-run] [--skip-check] [--skip-embassy]"
		echo ""
		echo "选项:"
		echo "  --dry-run        仅模拟发布，不实际上传"
		echo "  --skip-check     跳过发布前检查"
		echo "  --skip-embassy   跳过 kcp2-embassy（需 Rust 1.85+）"
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
header() {
	echo ""
	echo -e "${CYAN}========================================${NC}"
	echo -e "${CYAN}  $1${NC}"
	echo -e "${CYAN}========================================${NC}"
}

ok() { echo -e "  ${GREEN}✓${NC} $1"; }
err() { echo -e "  ${RED}✗${NC} $1"; }
info() { echo -e "  ${CYAN}→${NC} $1"; }

die() {
	echo -e "\n${RED}${BOLD}✗ 发布中止: $1${NC}"
	exit 1
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
	[ -n "${ORIGINAL_TOOLCHAIN:-}" ] && rustup default "$ORIGINAL_TOOLCHAIN" 2>/dev/null || true
}
trap restore_toolchain EXIT

header "发布准备"
VERSION=$(grep '^version = ' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
echo -e "  版本:         ${BOLD}$VERSION${NC}"
echo -e "  原始工具链:   $ORIGINAL_TOOLCHAIN"
echo -e "  std 工具链:   $STD_TOOLCHAIN"
echo -e "  embassy 工具链: ${EMBASSY_TOOLCHAIN:-未安装}"
echo -e "  模拟模式:     $DRY_RUN"
echo ""

if $DRY_RUN; then
	info "dry-run 模式：不会实际上传到 crates.io"
fi

# ---------------------------------------------------------------------------
# 发布前检查
# ---------------------------------------------------------------------------
if ! $SKIP_CHECK; then
	rustup default "$STD_TOOLCHAIN" 2>/dev/null || true
	info "使用 $(rustc --version)"

	header "1. cargo check"
	cargo check --workspace --exclude kcp2-embassy --all-targets || die "cargo check 失败"
	if ! $SKIP_EMBASSY && [ -n "$EMBASSY_TOOLCHAIN" ]; then
		rustup default "$EMBASSY_TOOLCHAIN" 2>/dev/null || true
		cargo check -p kcp2-embassy --all-targets || die "kcp2-embassy check 失败"
		rustup default "$STD_TOOLCHAIN" 2>/dev/null || true
	fi
	ok "cargo check 通过"

	header "2. cargo test"
	cargo test --workspace --exclude kcp2-embassy || die "cargo test (default) 失败"
	cargo test --workspace --exclude kcp2-embassy --features aead || die "cargo test (aead) 失败"
	cargo test --workspace --exclude kcp2-embassy --features dtls || die "cargo test (dtls) 失败"
	ok "cargo test 通过 (default + aead + dtls)"

	header "3. cargo clippy"
	cargo clippy --workspace --exclude kcp2-embassy --all-targets -- \
		-D clippy::all -A clippy::pedantic || die "clippy 失败"
	ok "clippy 通过"

	header "4. cargo doc"
	cargo doc -p kcp2-core -p kcp2-std --no-deps || die "doc 生成失败"
	ok "doc 生成通过"
else
	info "跳过发布前检查 (--skip-check)"
fi

# ---------------------------------------------------------------------------
# 发布（按依赖顺序）
# ---------------------------------------------------------------------------
PUBLISH_ARGS="--allow-dirty $REGISTRY"
if $DRY_RUN; then
	PUBLISH_ARGS="--dry-run $PUBLISH_ARGS"
fi

publish_crate() {
	local name="$1"
	local toolchain="$2"
	local extra_args="${3:-}"

	header "发布 $name v$VERSION"
	rustup default "$toolchain" 2>/dev/null || true
	info "工具链: $(rustc --version)"

	if cargo publish -p "$name" $PUBLISH_ARGS $extra_args 2>&1; then
		ok "$name 发布成功"
	else
		err "$name 发布失败"
		if $DRY_RUN; then
			err "dry-run 模式，继续检查下一个..."
		else
			die "$name 发布失败，中止"
		fi
	fi
}

publish_crate "kcp2-core" "$STD_TOOLCHAIN"
publish_crate "kcp2-std" "$STD_TOOLCHAIN"

if ! $SKIP_EMBASSY; then
	if [ -n "$EMBASSY_TOOLCHAIN" ]; then
		publish_crate "kcp2-embassy" "$EMBASSY_TOOLCHAIN"
	else
		info "跳过 kcp2-embassy (无 Rust 1.85+ 工具链，用 --skip-embassy 显式跳过)"
	fi
fi

publish_crate "kcp2" "$STD_TOOLCHAIN"

# ---------------------------------------------------------------------------
# 完成
# ---------------------------------------------------------------------------
header "发布完成"

if $DRY_RUN; then
	info "这是 dry-run 模式，未实际上传"
	echo ""
	info "实际发布命令: $0"
else
	echo -e "  ${GREEN}${BOLD}✓ 全部发布成功！${NC}"
	echo ""
	echo -e "  ${CYAN}已发布:${NC}"
	echo -e "    kcp2-core   ${GREEN}$VERSION${NC}"
	echo -e "    kcp2-std    ${GREEN}$VERSION${NC}"
	if ! $SKIP_EMBASSY && [ -n "$EMBASSY_TOOLCHAIN" ]; then
		echo -e "    kcp2-embassy ${GREEN}$VERSION${NC}"
	fi
	echo -e "    kcp2        ${GREEN}$VERSION${NC}"
	echo ""
	echo -e "  ${CYAN}安装: cargo add kcp2${NC}"
fi
