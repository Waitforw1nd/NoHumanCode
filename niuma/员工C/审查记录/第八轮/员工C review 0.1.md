# C-R8-01候选审查0.1（需修，未验收）

2026-09-27经理GPT-6；固定7b5fb0a3f4c8421fdaeace2c362c50c8d19d4568的server/CLI首批接入，已读HTTP完整差异及CLI网络消费。尚未编译，不作功能通过。

N07-C01 / P1：CLI preview以Url::origin().ascii_serialization比较Host origin，前者省略默认80/443，契约及D validate_target则固定保留effective port，导致正常默认端口preview被拒。要求复用D规范化、不改Host；补真实CLI+mock Host的HTTP80/HTTPS443/IPv6/大小写等价成功和错误origin拒绝。C已确认并修复在途，未有测试结果。

确认StrictObject拒递归重复字段、新鉴权GET与mutation分开、NetworkError保留calls方向正确；既有guard/提取错误兼容按经理接口补充1。其他正式断言尤其unknown出口、command ack等待和web不自动再确认仍须真实测试。C组合6bd6725含真实B Store/schema/D Provider，但B Engine/Approval仍待接，不能作为可编译完成。最终门禁仍C唯一固定组合串行。
