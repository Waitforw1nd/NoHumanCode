# NEXT-13 D固定候选独立只读交叉review

2026-09-28，员工C/GPT-6-astra，C-R14-01-20260927-E01附加只读范围。固定D提交 **fc50176896d96ccbcc9206834376e77f82a0c745**（已git rev-parse与git show核全SHA），只看该commit对象，不用D在途树。C源码仍9c996a6a00e1657afd649ab6a70da41322ce8a99，status/index空，无构建。未运行测试或攻击fixture、未改任何源码；以下为静态结论。没有重读经理身份历史。

## 结论：CHANGES_REQUIRED，一项新的P1

### N13-CD01：盘根Project的可执行排除失效，claim前可启动项目内git.exe

位置均为固定fc50176的 NoManCode/rust-app/src/git_worktree.rs：within 1161–1168，path_key 1147–1159，command 337–352；inspect_with首条Git 580，git→run→spawn链281附近。

within先path_key，然后对root无条件push分隔符。Windows盘根经canonicalize为标准扩展盘根（例如以盘符X表示任意已登记盘）：path_key去扩展前缀后仍是 `x:\`；再次push得到两个连续分隔符。候选 `x:\tools\git.exe` 的规范字符串只在盘符后含一个分隔符，既不等于root，也不starts_with双分隔符前缀，因此被误判“不在盘根内”。Unix fixture根 `/` 同理被拼成 `//`，但本发现优先针对正式Windows目标。

前置可达性：workspace_identity.rs validate_path约166起允许绝对Disk/VerbatimDisk根，并没有要求Normal组件；capture/verify检验目录身份不会额外拒绝盘根。project_config.rs399–439的注册入口检查metadata path、RootIdentity和已有Project overlap，没有禁盘根。secrets::safe_metadata_path只拒空/控制/敏感文本。因此可有两个合法、不重叠Project：盘根X，以及另一盘Y上的源repo。

具体触发：Host PATH包含可信绝对目录X盘中的tools目录（PATH本身可以早已存在，无需请求改环境），之后X盘根被登记为Project；在其可写范围内存在/替换git.exe。创建Y盘源repo的worktree时，完整GitContext确实含X盘根，但command第349行使用有缺陷的within排除，选择该候选；inspect_with第580行的第一条rev-parse经run.spawn在durable claim前启动此exe。即使伪Git不返回有效Git输出，外部marker/进程副作用已经发生，后续预检拒绝无法撤销；这违反“任一登记Project可写根内git.exe拒绝”和claim前只读边界。对只读diagnose亦可沿command触发，因此不是仅目标创建重叠的显示问题。

建议由D修within的根分隔符/组件语义，保留Windows别名规范化，并以盘根边界反例和项目git.exe marker正反控验证。不要求C写跨owner测试。修复不应仅禁止正常兄弟parent，也不应把真实项目内exe风险归因于PATH可信输入而略过：契约明确在可信PATH基础上仍必须排除Project范围。

证据限制：本轮仅静态审查，没有在机器盘根登记Project、放置可执行或启动攻击程序；上述执行链依据固定源码。现有w05_project_git_executable_and_inherited_environment_are_excluded使用普通temp/source根，不能覆盖盘根尾分隔符。建议经理登记新条目并交D唯一修订，编号可按主台账调整。

## 其余边界核对（未见新的具体阻断）

- command使用绝对canonical executable与Command参数，不经shell；env_clear后只回填PATH/Windows系统/TMP，屏蔽Git注入环境，固定no global/system、protocol、lazy fetch、replace、credentials、diff、fsmonitor及maintenance等。上述CD01之外，普通目录Project路径排除及全部ctx保留是现有设计，不重复报D01/D05。
- configuration先按已核common/config绝对路径执行--no-includes --null --list，检查全部多值，拒include/includeIf/filter/extensions/promisor/submodule/alternates等；危险配置检查在clean status之前。global attrs禁用，本地filter能力由配置拒绝。未发现另一个确定可达的外部命令配置路径；这不是对未来全部Git配置的安全证明。
- claim前没有aux_policy调用；execute_claimed先复核再创建独占辅助目录/空hook与属性文件。claim后add使用实际存在且复核的空policy。配置/path检查后恶意外部并发替换不在本契约OS沙箱保证内，未将该已声明限制重复报为新漏洞。
- run在spawn成功后mutating=true的非零、输出错误/超界、timeout全转OutcomeUnknown；execute_claimed后验所有错误亦map为OutcomeUnknown。未发现D将已spawn失败转换明确Rejected的路径。spawn本身失败返回Unavailable属于无子进程分支；预约释放由B消费，不在D模块直接执行。
- diagnose只做身份、受控只读Git与metadata观测，不调用aux_policy、add或状态/预约落库；registered_result_root仅重叠豁免，不从ctx删root。保存admin第四参在线比较已在固定候选；dirty/无关offline/admin残留等既有D01–05不重复泛报，D06 Clippy属于经理已登记在修。
- run cleanup保守unknown且不承诺所有后代全灭；未将该明确边界扩成完整进程树隔离。W04/W05尚待D补充动态证据，本review不宣称其未执行测试通过。

## 交付与下一步

本报告仅交叉review，不代替D自测/经理验收。C继续源码/no-build冻结；先由经理裁决并交D CD01修订，C收到新固定SHA后按明确范围只读核差异。身份已追加本轮记录，无共享index操作。
