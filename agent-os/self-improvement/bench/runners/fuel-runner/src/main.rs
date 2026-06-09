//! Deterministic fuel meter for kernel_bench.wasm. Feeds the corpus file to
//! the guest on stdin, runs it under wasmtime with fuel metering, relays the
//! guest's DIGEST line, and prints FUEL consumed plus SCALAR baseline/consumed
//! when --baseline is given. Same wasm + same corpus -> bit-identical fuel.

use wasmtime::{Config, Engine, Linker, Module, Store};
use wasmtime_wasi::preview1::{self, WasiP1Ctx};
use wasmtime_wasi::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::WasiCtxBuilder;

const FUEL_CAP: u64 = 2_000_000_000_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: fuel-runner <kernel_bench.wasm> <corpus.bin> [--baseline N]");
        std::process::exit(2);
    }
    let baseline = args
        .iter()
        .position(|a| a == "--baseline")
        .map(|i| args[i + 1].parse::<u64>().expect("baseline u64"));

    let mut config = Config::new();
    config.consume_fuel(true);
    let engine = Engine::new(&config)?;
    let module = Module::from_file(&engine, &args[1])?;
    let mut linker: Linker<WasiP1Ctx> = Linker::new(&engine);
    preview1::add_to_linker_sync(&mut linker, |t| t)?;

    let corpus = std::fs::read(&args[2])?;
    let stdout = MemoryOutputPipe::new(1 << 20);
    let wasi = WasiCtxBuilder::new()
        .stdin(MemoryInputPipe::new(corpus))
        .stdout(stdout.clone())
        .inherit_stderr()
        .build_p1();
    let mut store = Store::new(&engine, wasi);
    store.set_fuel(FUEL_CAP)?;

    let instance = linker.instantiate(&mut store, &module)?;
    let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;
    start.call(&mut store, ())?;

    let consumed = FUEL_CAP - store.get_fuel()?;
    print!("{}", String::from_utf8_lossy(&stdout.contents()));
    println!("FUEL {consumed}");
    if let Some(b) = baseline {
        println!("SCALAR {:.6}", b as f64 / consumed as f64);
    }
    Ok(())
}
