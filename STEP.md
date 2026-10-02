# 进行的步骤
## 授予权限
1. 生产环境的数据库访问权限（仅访问，不可作出修改）
```text
postgres://root:webgateway@10.240.0.1:5432/postgres
```
2. 测试环境域名
```text
test-proxy.txit.top
test-vcmp.txit.top
tp.txit.top
```

3. 测试的证书
可以在生产环境中获取含有此域名的 `txit.top`

4. 生产环境的 ssh 权限授予（必须经过我的确认才能执行命令）
```text
10.240.0.1
```

## 目前
1. 我这个 `request_size_logs` 和 `response_size_logs` 是用于统计请求的大小，可以细化到每秒的颗粒度
2. 我希望就是可以一次性迁移，也就是说 gateway 或者是 dashboard 两者其中一个启动之后自动迁移（无感）

## 最后
1. 这个 `STEP.md` 的内容融合进去 `agent.md` 但是 `最后` 和 `授予权限` 可选做融合，必须让我确认你需要融合什么东西进去
2. 把当前已读取的内如保存到 `agent-context.md` 上，以便下次方便读取