#!/usr/bin/env just --justfile

release:
  cargo build --release    

lint:
  cargo clippy

build:
    cargo build

run +FLAGS='-m':
  cargo run -- {{FLAGS}}

run-release +FLAGS='-m':
  cargo run --release -- {{FLAGS}}

sleep:
    sleep 2