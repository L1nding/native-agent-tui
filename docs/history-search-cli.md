# 历史证据 CLI 搜索

使用 `--search` 在非交互环境检索当前工作区保留的脱敏证据：

```powershell
native-agent-tui --search waiting --journal-dir .\journal
native-agent-tui --search compaction --search-category compaction \
  --search-thread root-thread --search-turn turn-42
```

命令只读取已提交的 journal 前缀，会扫描所有当前保留 session。输出使用 `session#N` 别名和事件序号定位命中；用 `--sessions` 查看同一工作区的 session 列表。结果只包含证据类型、来源、范围、工具类别、身份是否存在、事件序号和有界字节统计，不输出 prompt、答案、秘密、命令、路径或原始工具结果。

`--search-category` 接受 `all`、`lifecycle`、`output`、`tool`、`compaction`、`request` 和 `waiting`；`--search-thread` 与 `--search-turn` 按服务端确认的身份过滤。三个筛选项只能和 `--search QUERY` 一起使用。

搜索不会启动 Codex、发送 RPC、回答请求或修改 journal。没有保留 session 时命令成功并显示空结果；查询超过 1024 个 UTF-8 字节、journal 损坏或历史读取失败时返回错误码 2。命中数量和保留元数据均有界，输出会报告被截断的命中和重复证据。

交互式历史页仍支持 `/` 或 `Ctrl+F`、F6 类别筛选和 Enter 定位。CLI 搜索复用同一个后台搜索内核，因此两种入口遵循相同的固定前缀、取消和脱敏规则。
