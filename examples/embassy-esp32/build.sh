#!/usr/bin/env bash
# =============================================================================
# ESP32 编译 + 烧录脚本
# 用法:
#   ./build.sh              # 仅编译 (S3)
#   ./build.sh flash        # 编译 + 烧录
#   ./build.sh monitor      # 编译 + 烧录 + 串口监控
#   ./build.sh clean        # 清除构建缓存
#   ./build.sh --chip c3    # 选择 ESP32-C3
#   ./build.sh --chip s3    # 选择 ESP32-S3 (默认)
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
EXAMPLE_NAME="embassy-esp32-example"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

# ---------------------------------------------------------------------------
# 默认参数
# ---------------------------------------------------------------------------
CHIP="s3"
ACTION="build" # build | flash | monitor
CLEAN=false

# ---------------------------------------------------------------------------
# 参数解析
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
	case "$1" in
	flash) ACTION="flash" ;;
	monitor) ACTION="monitor" ;;
	clean) ACTION="clean" ;;
	--chip)
		shift
		CHIP="${1#esp}"   # 允许 esp32s3 或 s3
		CHIP="${CHIP#32}" # 允许 32s3 或 s3
		;;
	--chip=*)
		CHIP="${1#*=}"
		CHIP="${CHIP#esp}"
		CHIP="${CHIP#32}"
		;;
	--help | -h)
		echo "用法: $0 [action] [options]"
		echo ""
		echo "Actions:"
		echo "  (default)  仅编译"
		echo "  flash      编译 + 烧录"
		echo "  monitor    编译 + 烧录 + 串口监控"
		echo "  clean      清除构建缓存"
		echo ""
		echo "Options:"
		echo "  --chip CHIP   目标芯片: s3 (默认) 或 c3"
		echo "  -h, --help    显示帮助"
		exit 0
		;;
	*)
		echo -e "${RED}未知参数: $1${NC}"
		exit 1
		;;
	esac
	shift
done

# ---------------------------------------------------------------------------
# 芯片配置
# ---------------------------------------------------------------------------
case "$CHIP" in
s3 | S3)
	CHIP_NAME="esp32s3"
	FEATURE="esp32s3"
	TARGET="xtensa-esp32s3-none-elf"
	TOOLCHAIN="esp"
	;;
c3 | C3)
	CHIP_NAME="esp32c3"
	FEATURE="esp32c3"
	TARGET="riscv32imc-unknown-none-elf"
	TOOLCHAIN="stable"
	;;
*)
	echo -e "${RED}不支持的芯片: $CHIP (支持: s3, c3)${NC}"
	exit 1
	;;
esac

# ---------------------------------------------------------------------------
# 工具路径
# ---------------------------------------------------------------------------
ESP_TOOLCHAIN_DIR="$HOME/.rustup/toolchains/esp"

if [ "$TOOLCHAIN" = "esp" ]; then
	CARGO_CMD="$ESP_TOOLCHAIN_DIR/bin/cargo"
	if [ ! -x "$CARGO_CMD" ]; then
		echo -e "${RED}未找到 esp 工具链: $CARGO_CMD${NC}"
		echo -e "${YELLOW}安装: https://github.com/esp-rs/rust-build${NC}"
		exit 1
	fi
	# Xtensa GCC 链接器
	XTENSA_GCC_BASE=$(find "$ESP_TOOLCHAIN_DIR/xtensa-esp-elf" -maxdepth 4 -name "xtensa-esp32s3-elf-gcc" -print -quit 2>/dev/null)
	if [ -n "$XTENSA_GCC_BASE" ]; then
		export PATH="$(dirname "$XTENSA_GCC_BASE"):$PATH"
	fi
	LIBCLANG_PATH="$ESP_TOOLCHAIN_DIR/xtensa-esp32-elf-clang/esp-20.1.1_20250829/esp-clang/lib"
	if [ -d "$LIBCLANG_PATH" ]; then
		export LIBCLANG_PATH
	fi
	export RUSTUP_TOOLCHAIN=esp
else
	CARGO_CMD="cargo"
fi

# ---------------------------------------------------------------------------
# 清除
# ---------------------------------------------------------------------------
if [ "$ACTION" = "clean" ]; then
	echo -e "${CYAN}清除构建缓存...${NC}"
	rm -rf "$SCRIPT_DIR/target/"
	rm -f "$SCRIPT_DIR/Cargo.lock"
	echo -e "${GREEN}✓ 已清除${NC}"
	exit 0
fi

# ---------------------------------------------------------------------------
# 编译
# ---------------------------------------------------------------------------
echo -e "${CYAN}========================================${NC}"
echo -e "${CYAN}  编译 $CHIP_NAME ($TARGET)${NC}"
echo -e "${CYAN}========================================${NC}"

BUILD_ARGS=(
	--manifest-path "$SCRIPT_DIR/Cargo.toml"
	--target "$TARGET"
	--features "$FEATURE"
	--release
)

if [ "$TOOLCHAIN" = "esp" ]; then
	BUILD_ARGS+=(-Z build-std=core,alloc)
fi

if $CARGO_CMD build "${BUILD_ARGS[@]}" 2>&1; then
	BINARY="$SCRIPT_DIR/target/$TARGET/release/$EXAMPLE_NAME"
	if [ -f "$BINARY" ]; then
		SIZE=$(stat --printf="%s" "$BINARY" 2>/dev/null || stat -f%z "$BINARY" 2>/dev/null)
		echo -e "${GREEN}✓ 编译成功 ($(($SIZE / 1024)) KB)${NC}"
	else
		echo -e "${YELLOW}⚠ 编译成功但未找到二进制文件${NC}"
	fi
else
	echo -e "${RED}✗ 编译失败${NC}"
	exit 1
fi

# ---------------------------------------------------------------------------
# 烧录
# ---------------------------------------------------------------------------
if [ "$ACTION" = "flash" ] || [ "$ACTION" = "monitor" ]; then
	echo ""
	echo -e "${CYAN}========================================${NC}"
	echo -e "${CYAN}  烧录到 $CHIP_NAME${NC}"
	echo -e "${CYAN}========================================${NC}"

	FLASH_ARGS=("flash")
	if [ "$ACTION" = "monitor" ]; then
		FLASH_ARGS+=("--monitor")
	fi
	FLASH_ARGS+=("$BINARY")

	if espflash "${FLASH_ARGS[@]}" 2>&1; then
		echo -e "${GREEN}✓ 烧录完成${NC}"
	else
		echo -e "${RED}✗ 烧录失败${NC}"
		exit 1
	fi
fi
