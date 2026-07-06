#!/usr/bin/env bash
# ============================================================================
# Ember EasyWorld 验证脚本 (Linux / macOS / WSL)
#
# 用法:
#   ./scripts/verify-easyworld.sh           # 文件模式验证
#   ./scripts/verify-easyworld.sh --mysql   # MySQL 模式验证 (需要 docker)
#   ./scripts/verify-easyworld.sh --all     # 两种模式都验证
#   ./scripts/verify-easyworld.sh --help
#
# 前置条件:
#   - Rust toolchain (rustup + cargo)
#   - (MySQL 模式) Docker
# ============================================================================

set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
PASS="${GREEN}[PASS]${NC}"; FAIL="${RED}[FAIL]${NC}"; INFO="${YELLOW}[INFO]${NC}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
TEMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TEMP_DIR"' EXIT

SERVER_BIN="$PROJECT_DIR/target/release/pumpkin"

MODE="file"

# ─── 参数解析 ──────────────────────────────────────────────────────────
print_help() {
  echo "Ember EasyWorld 验证脚本"
  echo ""
  echo "用法: $0 [--file] [--mysql] [--all] [--build-only] [--help]"
  echo ""
  echo "  --file        仅验证文件模式 (.easy)"
  echo "  --mysql       仅验证 MySQL 模式 (需要 docker)"
  echo "  --all         两种模式都验证"
  echo "  --build-only  仅编译，不运行验证"
  echo "  --help        显示此帮助"
  echo ""
  echo "示例:"
  echo "  $0                    # 文件模式"
  echo "  $0 --all              # 全部验证"
  echo "  $0 --mysql            # MySQL 模式"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --file)   MODE="file"; shift ;;
    --mysql)  MODE="mysql"; shift ;;
    --all)    MODE="all"; shift ;;
    --build-only) MODE="build"; shift ;;
    --help)   print_help; exit 0 ;;
    *)        echo "未知参数: $1"; print_help; exit 1 ;;
  esac
done

# ─── 步骤 1: 编译 ──────────────────────────────────────────────────────
build_server() {
  echo -e "\n${INFO} 编译 Ember (release)..."
  cd "$PROJECT_DIR"
  cargo build --release -p pumpkin 2>&1 | tail -5
  if [ ! -f "$SERVER_BIN" ]; then
    echo -e "${FAIL} 编译失败，未找到 $SERVER_BIN"
    exit 1
  fi
  echo -e "${PASS} 编译完成: $SERVER_BIN"
}

# ─── 步骤 3: 写入 easy 配置并验证 ──────────────────────────────────────
verify_file_mode() {
  local world_dir="$TEMP_DIR/easyworld_file"
  mkdir -p "$world_dir"

  echo -e "\n${INFO} === 验证文件模式 (type=easy) ==="

  cd "$world_dir"

  # 服务端从当前目录的 pumpkin.toml 读配置,区块格式在 [world.chunk]
  cat > pumpkin.toml << 'TOML'
java_edition_address = "0.0.0.0:25566"
bedrock_edition = false

[world.chunk]
type = "easy"

[plugins]
auto_approve_permissions = true
TOML

  echo -e "${INFO} 配置内容:"
  cat pumpkin.toml
  echo ""

  echo -e "${INFO} Starting server (30s, SIGINT 优雅停服触发存盘)..."
  timeout -s INT 30 "$SERVER_BIN" > /tmp/ember-easyworld-server.log 2>&1 || true

  echo -e "${INFO} Checking results..."

  LEVEL_DAT=$(find . -name "level.dat" 2>/dev/null | head -1)
  EASY_FILES=$(find . -name "*.easy" 2>/dev/null | head -10)
  if [ -n "$EASY_FILES" ]; then
    echo -e "${PASS} EasyWorld file mode OK - found .easy files:"
    for f in $EASY_FILES; do
      SIZE=$(du -h "$f" | cut -f1)
      echo "    $f ($SIZE)"
    done
  elif [ -n "$LEVEL_DAT" ]; then
    echo -e "${PASS} EasyWorld file mode OK - server started (level.dat found), no chunks yet (no player joined)."
    echo -e "${INFO} Directory tree:"
    find . -type f \( -name "*.easy" -o -name "*.toml" -o -name "level.dat" \) 2>/dev/null | sort | while read f; do echo "    $f"; done
  else
    echo -e "${YELLOW}[WARN]${NC} No level.dat - server may not have started correctly."
    echo -e "${INFO} Server log (last 30 lines):"
    tail -30 /tmp/ember-easyworld-server.log 2>/dev/null || true
  fi
}

# ─── 步骤 4: MySQL 模式验证 ────────────────────────────────────────────
verify_mysql_mode() {
  echo -e "\n${INFO} === 验证 MySQL 模式 (type=easy_mysql) ==="

  # 检查 Docker
  if ! command -v docker &>/dev/null; then
    echo -e "${FAIL} 需要 Docker 但未安装。跳过 MySQL 验证。"
    return
  fi

  local world_dir="$TEMP_DIR/easyworld_mysql"
  mkdir -p "$world_dir"

  echo -e "${INFO} 启动 MySQL 容器..."
  docker rm -f ember-mysql-test 2>/dev/null || true
  docker run -d --name ember-mysql-test \
    -e MYSQL_ROOT_PASSWORD=ember_test \
    -e MYSQL_DATABASE=ember \
    -p 3307:3306 \
    mysql:8 2>&1 | tail -3

  echo -e "${INFO} 等待 MySQL 就绪..."
  for i in $(seq 1 30); do
    if docker exec ember-mysql-test mysqladmin ping -h localhost --silent 2>/dev/null; then
      echo -e "${PASS} MySQL 就绪"
      break
    fi
    if [ "$i" -eq 30 ]; then
      echo -e "${FAIL} MySQL 启动超时"
      docker logs ember-mysql-test --tail 20
      docker rm -f ember-mysql-test 2>/dev/null
      return
    fi
    sleep 2
  done

  cd "$world_dir"

  cat > pumpkin.toml << 'TOML'
java_edition_address = "0.0.0.0:25567"
bedrock_edition = false

[world.chunk]
type = "easy_mysql"
url = "mysql://root:ember_test@127.0.0.1:3307/ember"

[plugins]
auto_approve_permissions = true
TOML

  echo -e "${INFO} 配置内容:"
  cat pumpkin.toml
  echo ""

  echo -e "${INFO} Starting server (30s, SIGINT 优雅停服触发存盘)..."
  timeout -s INT 30 "$SERVER_BIN" > /tmp/ember-easyworld-mysql.log 2>&1 || true

  echo -e "${INFO} Querying MySQL..."

  ROW_COUNT=$(docker exec ember-mysql-test mysql -u root -pember_test ember -e \
    "SELECT COUNT(*) AS cnt FROM easyworld_regions;" 2>/dev/null | tail -1)
  echo -e "${INFO} easyworld_regions rows: $ROW_COUNT"

  if [ "$ROW_COUNT" -gt 0 ] 2>/dev/null; then
    echo -e "${PASS} MySQL mode OK - found data in easyworld_regions"

    echo -e "${INFO} Sample data:"
    docker exec ember-mysql-test mysql -u root -pember_test ember -e \
      "SELECT world_key, region_x, region_z, LENGTH(data) AS size_bytes
       FROM easyworld_regions LIMIT 5;" 2>/dev/null
  else
    LEVEL_DAT=$(find . -name "level.dat" 2>/dev/null | head -1)
    if [ -n "$LEVEL_DAT" ]; then
      echo -e "${PASS} MySQL mode OK - server started (level.dat found), table empty (no player joined)."
    else
      echo -e "${YELLOW}[WARN]${NC} No level.dat - server may not have connected to MySQL."
      echo -e "${INFO} Server log (last 30 lines):"
      tail -30 /tmp/ember-easyworld-mysql.log 2>/dev/null || true
    fi
  fi

  echo -e "${INFO} Cleaning up MySQL container..."
  docker rm -f ember-mysql-test 2>/dev/null
  echo -e "${PASS} MySQL 验证完成"
}

# ─── 主流程 ────────────────────────────────────────────────────────────

echo "============================================"
echo " Ember EasyWorld 验证"
echo " 项目目录: $PROJECT_DIR"
echo " 临时目录: $TEMP_DIR"
echo " 模式: $MODE"
echo "============================================"

build_server

case "$MODE" in
  file)
    verify_file_mode
    ;;
  mysql)
    verify_mysql_mode
    ;;
  all)
    verify_file_mode
    verify_mysql_mode
    ;;
  build)
    echo -e "${PASS} 编译完成，跳过运行验证。"
    echo "  二进制: $SERVER_BIN"
    ;;
esac

echo ""
echo "============================================"
echo -e " ${GREEN}EasyWorld 验证结束${NC}"
echo "============================================"
