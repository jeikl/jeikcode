# 用户提问包装模板 (User Query Wrapper Template)
#
# 占位符说明：
# {{input}} : 运行时将被替换为用户在当前轮次实际输入的 prompt 文本。
#
# 优先级顺序：
# 1. 项目级: <workspace>/.atomcode/user-wrap.md
# 2. 项目级: <workspace>/user-wrap.md
# 3. 全局级: ~/.atomcode/user-wrap.md
#
# 特性规则：
# - 仅对用户发送的最后一条真实 User Prompt 生效；
# - 内部交互、系统提示词插入、记忆注入、工具调用过程等均不处理；
# - 动态热重载：文件修改后下一轮提问即刻生效，无须重启；
# - 默认保持 {{input}} 原样透传。

{{input}}
