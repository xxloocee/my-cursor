#!/bin/bash
APP_PATH="/Applications/My Cursor.app"

if [ ! -d "$APP_PATH" ]; then
  echo "请先把 My Cursor 拖到“应用程序”目录后再运行本脚本。"
  read -r -p "按回车退出..."
  exit 1
fi

echo "正在移除隔离属性: $APP_PATH"
xattr -cr "$APP_PATH"
echo "处理完成，现在可以正常打开 My Cursor。"
read -r -p "按回车退出..."
