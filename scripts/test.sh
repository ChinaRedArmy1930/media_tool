#!/usr/bin/env bash

set -e  # 遇到错误立即退出

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}  FFmpeg Split Audit - 音频提取测试${NC}"
echo -e "${GREEN}========================================${NC}"

# 项目根目录
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"

# 测试文件配置
TEST_VIDEO_URL="https://download.blender.org/demo/movies/BBB/bbb_sunflower_1080p_30fps_normal.mp4.zip"
TEST_VIDEO_NAME="test_video.mp4"
TEST_VIDEO_ZIP="bbb_sunflower_1080p_30fps_normal.mp4.zip"
TEST_OUTPUT_DIR="test_output"
AUDIO_OUTPUT_BASE="$TEST_OUTPUT_DIR/extracted_audio" 

echo -e "\n${YELLOW}[1/5] 准备测试环境...${NC}"
mkdir -p "$TEST_OUTPUT_DIR"

# 检查测试视频是否存在，不存在则下载并解压
if [ ! -f "$TEST_OUTPUT_DIR/$TEST_VIDEO_NAME" ]; then
    echo -e "${YELLOW}测试视频不存在，开始下载...${NC}"
    echo -e "URL: $TEST_VIDEO_URL"
    
    # 检查 unzip 是否可用
    if ! command -v unzip &> /dev/null; then
        echo -e "${RED}错误: 需要 unzip 命令来解压文件${NC}"
        exit 1
    fi
    
    # 优先使用 curl，其次 wget
    if command -v curl &> /dev/null; then
        curl -L -o "$TEST_OUTPUT_DIR/$TEST_VIDEO_ZIP" "$TEST_VIDEO_URL" \
            --progress-bar \
            || { echo -e "${RED}下载失败！${NC}"; exit 1; }
    elif command -v wget &> /dev/null; then
        wget -O "$TEST_OUTPUT_DIR/$TEST_VIDEO_ZIP" "$TEST_VIDEO_URL" \
            || { echo -e "${RED}下载失败！${NC}"; exit 1; }
    else
        echo -e "${RED}错误: 需要 curl 或 wget 来下载测试文件${NC}"
        exit 1
    fi
    
    echo -e "${GREEN}✓ 下载完成${NC}"
    
    # 解压 zip 文件
    echo -e "${YELLOW}正在解压文件...${NC}"
    unzip -q -o "$TEST_OUTPUT_DIR/$TEST_VIDEO_ZIP" -d "$TEST_OUTPUT_DIR/" \
        || { echo -e "${RED}解压失败！${NC}"; exit 1; }
    
    # 查找解压出来的 mp4 文件并重命名
    EXTRACTED_MP4=$(find "$TEST_OUTPUT_DIR" -name "*.mp4" -type f | head -n 1)
    
    if [ -z "$EXTRACTED_MP4" ]; then
        echo -e "${RED}错误: 在 zip 文件中未找到 mp4 文件${NC}"
        exit 1
    fi
    
    # 如果解压出的文件名与目标不同，则重命名
    if [ "$EXTRACTED_MP4" != "$TEST_OUTPUT_DIR/$TEST_VIDEO_NAME" ]; then
        mv "$EXTRACTED_MP4" "$TEST_OUTPUT_DIR/$TEST_VIDEO_NAME"
    fi
    
    # 清理 zip 文件
    rm -f "$TEST_OUTPUT_DIR/$TEST_VIDEO_ZIP"
    
    echo -e "${GREEN}✓ 解压完成${NC}"
else
    echo -e "${GREEN}✓ 测试视频已存在${NC}"
fi


echo -e "\n${YELLOW}[2/5] 构建项目...${NC}"
cargo build --release || { echo -e "${RED}构建失败！${NC}"; exit 1; }
echo -e "${GREEN}✓ 构建成功${NC}"

echo -e "\n${YELLOW}[3/5] 分析测试视频...${NC}"
echo -e "${YELLOW}─────────────────────────────────────────${NC}"
cargo run --release -- \
    --input "$TEST_OUTPUT_DIR/$TEST_VIDEO_NAME" \
    || { echo -e "${RED}分析失败！${NC}"; exit 1; }
echo -e "${YELLOW}─────────────────────────────────────────${NC}"

echo -e "\n${YELLOW}[4/5] 执行音频提取测试...${NC}"
echo -e "${YELLOW}─────────────────────────────────────────${NC}"

# 清理之前可能存在的音频文件目录
if [ -d "$AUDIO_OUTPUT_BASE" ]; then
    rm -rf "$AUDIO_OUTPUT_BASE"
    echo -e "清理旧的输出目录..."
fi

cargo run --release -- \
    --input "$TEST_OUTPUT_DIR/$TEST_VIDEO_NAME" \
    --audio-output-path "$AUDIO_OUTPUT_BASE" \
    || { echo -e "${RED}音频提取失败！${NC}"; exit 1; }
echo -e "${YELLOW}─────────────────────────────────────────${NC}"

# 验证输出文件（查找生成的音频目录和文件）
echo -e "\n${BLUE}检查生成的音频文件...${NC}"

if [ ! -d "$AUDIO_OUTPUT_BASE" ]; then
    echo -e "${RED}✗ 音频输出目录未创建：$AUDIO_OUTPUT_BASE${NC}"
    exit 1
fi

# 查找所有生成的音频文件
AUDIO_FILES=($(find "$AUDIO_OUTPUT_BASE" -name "audio_*.*" -type f | sort))

if [ ${#AUDIO_FILES[@]} -eq 0 ]; then
    echo -e "${RED}✗ 在 $AUDIO_OUTPUT_BASE 目录中未找到任何音频文件！${NC}"
    exit 1
fi

echo -e "${GREEN}✓ 找到 ${#AUDIO_FILES[@]} 个音频文件：${NC}"
TOTAL_SIZE=0
for audio_file in "${AUDIO_FILES[@]}"; do
    # 兼容 macOS 和 Linux 的 stat 命令
    if [[ "$OSTYPE" == "darwin"* ]]; then
        FILE_SIZE=$(stat -f%z "$audio_file" 2>/dev/null)
    else
        FILE_SIZE=$(stat -c%s "$audio_file" 2>/dev/null)
    fi
    FILE_SIZE_HUMAN=$(du -h "$audio_file" | cut -f1)
    FILENAME=$(basename "$audio_file")
    echo -e "  ${BLUE}•${NC} $FILENAME (大小: $FILE_SIZE_HUMAN)"
    TOTAL_SIZE=$((TOTAL_SIZE + FILE_SIZE))
done

# 检查文件是否有实际内容（不是空文件）
if [ $TOTAL_SIZE -lt 1024 ]; then
    echo -e "${RED}✗ 音频文件太小（总计 ${TOTAL_SIZE} 字节），可能提取失败！${NC}"
    exit 1
fi

TOTAL_SIZE_MB=$((TOTAL_SIZE / 1024 / 1024))
echo -e "${GREEN}  总大小: ${TOTAL_SIZE_MB} MB${NC}"

echo -e "\n${YELLOW}[5/5] 清理测试文件...${NC}"
rm -rf "$AUDIO_OUTPUT_BASE"
echo -e "${GREEN}✓ 已清理音频输出目录: $AUDIO_OUTPUT_BASE${NC}"

echo -e "\n${GREEN}========================================${NC}"
echo -e "${GREEN}  ✓ 所有测试通过！${NC}"
echo -e "${GREEN}========================================${NC}"

echo -e "\n${YELLOW}测试摘要：${NC}"
echo -e "  • 提取音频流数量: ${BLUE}${#AUDIO_FILES[@]}${NC}"
echo -e "  • 音频总大小: ${BLUE}${TOTAL_SIZE_MB} MB${NC}"
echo -e "  • 测试视频保留在: ${BLUE}$TEST_OUTPUT_DIR/$TEST_VIDEO_NAME${NC}"
echo -e "\n${YELLOW}清理命令：${NC}"
echo -e "  • 重新下载测试视频: ${YELLOW}rm $TEST_OUTPUT_DIR/$TEST_VIDEO_NAME${NC}"
echo -e "  • 清理所有测试数据: ${YELLOW}rm -rf $TEST_OUTPUT_DIR${NC}"