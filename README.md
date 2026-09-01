# rterm

带集成式 SFTP 文件管理器的跨平台终端 / SSH 客户端。

- 终端基于 [iced_term](https://github.com/kemokempo/iced_term) 改造
- 应用图标来自 [OpenSVG](https://opensvg.dev/)

## 运行

```bash
# 调试构建并运行
cargo run

# 发布构建
cargo build --release
# 二进制位于 target/release/rterm
```

## 打包

使用 [`cargo-deb`](https://crates.io/crates/cargo-deb) 生成 Debian 包，使用 [`cargo-generate-rpm`](https://crates.io/crates/cargo-generate-rpm) 生成 RPM 包：

```bash
cargo install cargo-deb cargo-generate-rpm --locked

# 生成 .deb
cargo deb

# 生成 .rpm
cargo generate-rpm
```

## 许可证

MIT —— 见 [LICENSE](LICENSE)。
