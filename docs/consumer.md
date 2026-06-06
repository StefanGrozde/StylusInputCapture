# Reference consumer

The `tablet-stream` crate includes a small Rust consumer example that decodes
the live stream and prints capabilities, sample rows, proximity events, and
metrics summaries.

## Stdout postcard stream

The default CLI transport writes framed postcard bytes to stdout. Pipe that
directly into the consumer:

```powershell
cargo run -p tablet-cli -- --transport stdout | cargo run -p tablet-stream --example consumer
```

Sample rows are printed as:

```text
sample,t_capture_ns,x_raw,y_raw,pressure_raw,serial
sample,5000000,0,0,0,1
```

## TCP JSON stream

Start the producer in one terminal:

```powershell
cargo run -p tablet-cli -- --transport tcp --format json
```

Connect the consumer from another terminal:

```powershell
cargo run -p tablet-stream --example consumer -- --tcp 127.0.0.1:9123 --format json
```

## Flags

```text
--tcp <addr>              Connect to a TCP stream instead of reading stdin
--format <postcard|json>  Decode framed postcard binary or JSONL
-h, --help                Show help
```
