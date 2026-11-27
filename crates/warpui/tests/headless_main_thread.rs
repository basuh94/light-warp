use libtest_mimic::{Arguments, Trial};

fn main() {
    let mut args = Arguments::from_args();
    // This test must run on the process main thread to service the macOS run loop.
    args.test_threads = Some(1);

    let tests = Vec::<Trial>::new();

    libtest_mimic::run(&args, tests).exit();
}
