# fadb 改名与发布 Checklist

> 项目更名为 **fadb** — a featherweight ADB toolbox, in Rust。
> 状态核对于 2026-09-06：改名主体工作已完成，剩余项集中在"正式上 crates.io"与"推广"。

## 一、先锁定资产（半小时内搞定，防抢注）

- [ ] 注册域名 **fadb.dev**（可选 + fadb.rs）
- [x] 在 crates.io 上占位：`cargo new fadb && cargo publish`（发布一个 0.0.0-placeholder 版本，描述写清楚，避免被抢注——**这步很关键，crate 名先到先得**）✅ 2026-09-06 已发布 [fadb 0.0.0](https://crates.io/crates/fadb)，名字已锁定
- [ ] npm 占位（可选）：`npm publish` 一个空 `fadb` 包（名字未被占用;本机 npm 未登录,需先 `npm adduser`）
- [ ] GitHub 上注册组织 `fadb-dev` 之类备用（`fadb` 用户名已被占，确认下它是不是僵尸号，是的话可以试着向 GitHub 申诉释放，但别抱希望）

## 二、GitHub 仓库改名

- [x] Settings → Rename 仓库为 `fadb`（旧链接自动 301 跳转，star / issue / PR / fork 全部保留）✅ 已完成，CHANGELOG 保留更名记录
- [x] 改仓库 description 和 topics（加上 `adb` `android` `rust` `egui` `devtools`）✅ 已配置
- [x] 本地 remote 更新：`git remote set-url origin git@github.com:yeqing17/fadb.git`（不改也能用，跳转有效，但建议改）✅ 已指向新地址

## 三、代码层改名（工作量主要在这）

- [x] `Cargo.toml` workspace：crate 名改为 `fadb-desktop`（或 `fadb-gui`），**目录名一并改** ✅ 7 个库 crate 均为 fadb-*；桌面包名后于 2026-09-06 进一步改为 `fadb`（`cargo install fadb` 认包名），目录 `apps/fadb-desktop` 保留
- [x] `cargo fmt / clippy / test / build` 四条命令全跑一遍，确认 workspace 改名后无残留引用 ✅ CI（ci.yml）持续覆盖；`git grep -i bridgescope` 仅命中 CHANGELOG 的历史更名记录，属有意保留
- [x] 全局搜索替换（注意区分大小写）：
  - 大驼峰显示名 → `Fadb`（UI 显示名、窗口标题）✅ app.rs 窗口标题、头部标识均为 Fadb
  - 小写标识符 / 路径 → `fadb` ✅
  - 环境变量 → `FADB_FAKE`（README 里也要同步改）✅ 中英 README 与代码一致
- [x] 检查 `docs/clean-room.md`、`docs/feature-matrix.md` 等文档里的项目名引用 ✅ 无旧名残留
- [x] 配置文件 / 缓存目录名：检查代码里有没有往 `~/.config/xxx` 这类路径写东西；改名意味着用户旧配置会"丢失"，要么做迁移逻辑，要么在 release note 里说明 ✅ 代码无旧名配置路径；BridgeScope 时期（0.4.x–0.6.x）老用户配置不迁移，CHANGELOG 更名条目即说明

## 四、README 和门面（决定爆款相的部分）

- [x] 标题换成 `fadb`，副标题上 slogan：**a featherweight ADB toolbox, in Rust** ✅ social-preview.png + README 双语
- [x] 顶部加 badges：crates.io version、license、CI status、downloads ✅ 已有 version / rust / GUI / platform / CI / license 六枚；crates.io 版徽章等正式发布后加（现在只有 0.0.0 占位，挂着不好看）
- [x] **补一张好看的截图或 GIF 放最顶上**——GUI 工具没有 demo 图，star 转化率差一个数量级 ✅ social-preview.png
- [x] 安装方式加上 `cargo install fadb`（等正式发布后）✅ README 中英双版已加，`fadb` 0.8.11 已在 crates.io
- [x] 清理 README 里所有旧项目名的历史描述 ✅

## 五、发布与推广（改完名才是开始）

- [x] 打一个 **v0.8.0**（改名本身就值得一个 minor version），release note 里写明已完成更名 ✅ v0.8.12 起双平台同步:GitHub Release 产物为 `fadb-` 前缀新命名,crates.io 同版本发布
- [x] `cargo publish` 正式版 ✅ 2026-09-06 全部 8 个包发布 0.8.11。发布经验（下次发版照做）：
  - 本机 rsproxy 镜像会**劫持依赖解析**（报 "no matching package found"），发布要用无 config 的临时 CARGO_HOME（把 `F:\DevCache\cargo\credentials.toml` 拷进去）：`CARGO_HOME=<临时目录> cargo publish -p <crate> --registry crates-io`
  - crates.io 对**新 crate 限流约 1 个/10 分钟**（429），首次发多个新包要按窗口逐个发；老 crate 发新版本不受此限
  - 依赖版本字面量：各 manifest 里 path 依赖写死 `version = "0.8.12"`（14 处），升版时同步改；0.8.x 内不改也兼容（caret 语义），跨 0.9 必须改
- [ ] 发帖渠道按效果排序：
  1. **r/rust** 的 "What's everyone working on" 帖或直接发 showcase（GUI 工具带截图在 r/rust 很吃香）
  2. **This Week in Rust** 提交
  3. V2EX / 掘金 / 少数派（中文圈）
  4. X / 即刻，带 #rustlang #androiddev 标签
- [ ] 提交到 awesome-rust、awesome-adb 这类列表（提 PR 即可，免费流量）

## 两个坑提前说

1. **改名后 24h 内别大规模宣传**——GitHub 跳转缓存、crates.io 索引都有延迟，等链接全部生效再发（GitHub 侧早已过窗口期；crates.io 待正式发布后再宣传）
2. **改名和发版分开做 commit**——万一改名引入 bug，方便 bisect 定位
