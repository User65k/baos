FROM rust:1.83.0-slim-bullseye 
 
RUN apt update && apt upgrade -y 
RUN apt install -y g++-arm-linux-gnueabihf libc6-dev-armhf-cross

RUN rustup target add arm-unknown-linux-gnueabihf
 
RUN dpkg --add-architecture armhf 
RUN apt update
#RUN apt install --assume-yes libdbus-1-dev libdbus-1-dev:armhf pkg-config

#WORKDIR /app 
 
ENV CARGO_TARGET_arm-unknown-linux-gnueabihf_LINKER=arm-linux-gnueabihf-gcc 
ENV CC_arm-unknown-linux-gnueabihf=arm-linux-gnueabihf-gcc 
ENV CXX_arm-unknown-linux-gnueabihf=arm-linux-gnueabihf-g++
ENV CARGO_TARGET_arm-unknown-linux-gnueabihf_LINKER=/usr/bin/arm-linux-gnueabihf-gcc
ENV PKG_CONFIG_ALLOW_CROSS="true"
ENV PKG_CONFIG_PATH="/usr/lib/arm-linux-gnueabihf/pkgconfig"
ENV RUSTFLAGS="-L /usr/arm-linux-gnueabihf/lib/ -L /usr/lib/arm-linux-gnueabihf/ -C link-args=-marm -C link-args=-march=armv6zk -C link-args=-mtune=arm1176jzf-s -C link-args=-mfpu=vfp -C link-args=-mfloat-abi=hard"
