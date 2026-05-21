"""micu-image-mcp 内部 package。

server.py 是用户/MCP 客户端的 entry point；本 package 是其内部实现。
拆分依据：纯函数 / IO / 锁 / HTTP / 保存逻辑各自一个模块，server.py 只保留
mcp = FastMCP("...") 注册 + 5 个 @mcp.tool() 函数 + main()。

所有符号沿用原 server.py 命名（含 `_` 私有前缀），保证 tests/_common 通过
`import server` 找到的符号集合不变。
"""
