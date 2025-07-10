FROM rust:1.78-alpine as builder

# 安装构建依赖
RUN apk add --no-cache \
    musl-dev \
    pkgconfig \
    fuse3-dev \
    fuse3-static \
    build-base \
    linux-headers

# 设置工作目录
WORKDIR /app

# 复制 Cargo 文件
COPY Cargo.toml Cargo.lock ./

# 复制源代码
COPY src ./src

# 设置环境变量进行静态链接
ENV RUSTFLAGS="-C target-feature=+crt-static -C link-arg=-static"
ENV PKG_CONFIG_ALL_STATIC=1

# 构建项目
RUN cargo build --release --target x86_64-unknown-linux-musl

# 创建最终镜像
FROM alpine:latest

# 安装运行时依赖
RUN apk add --no-cache fuse3

# 复制构建的二进制文件
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/nfs-cachefs /usr/local/bin/

# 设置权限
RUN chmod +x /usr/local/bin/nfs-cachefs

# 创建挂载点
RUN mkdir -p /mnt/nfs /mnt/cache /mnt/cached

# 暴露端口（如果需要）
EXPOSE 2049

# 设置入口点
ENTRYPOINT ["/usr/local/bin/nfs-cachefs"]
CMD ["--help"] 