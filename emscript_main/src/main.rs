use emscript_main::{
    dag_program::{cf_build::compile_program, human_format::parse_mir_program},
    runtime::Gc,
};

fn run_example_mul2add1() {
    let s = include_str!("dag_program/mir_mul2add1.txt");
    let program = parse_mir_program(s, "i32_mul2add11");
    let compiled = compile_program(&program);
    let f: extern "sysv64" fn(i32) -> i32 = unsafe { compiled.get_function("i32_mul2add11") };
    for i in 0..10 {
        println!("{i} -> {}", f(i));
    }
}

fn run_example_fib() {
    let s = include_str!("dag_program/mir_fib.txt");
    let program = parse_mir_program(s, "fibonacci");
    let compiled = compile_program(&program);
    let fib_recurse: extern "sysv64" fn(i32) -> i32 = unsafe { compiled.get_function("fibonacci") };
    let fib_iter: extern "sysv64" fn(i32) -> i32 =
        unsafe { compiled.get_function("fibonacci_iter") };
    println!("=====Recursive=====");
    for i in 0..10 {
        println!("{i} -> {}", fib_recurse(i));
    }
    println!("=====Iterative=====");
    for i in 0..10 {
        println!("{i} -> {}", fib_iter(i));
    }
}

fn run_example_refs() {
    let s = include_str!("dag_program/mir_refs.txt");
    let program = parse_mir_program(s, "return_ref");
    let compiled = compile_program(&program);

    let ret_ref: extern "sysv64" fn(i32) -> i64 = unsafe { compiled.get_function("return_ref") };
    let ret_ref_struct: extern "sysv64" fn(i32) -> i64 =
        unsafe { compiled.get_function("return_ref_struct") };

    let mut gcs = vec![];

    for i in 0..10 {
        let ret_addr = ret_ref(i);
        let ret_gc = unsafe { Gc::new(ret_addr as *mut i32) };
        gcs.push(ret_gc);
    }

    for i in 0..10 {
        println!("{i} -> {} at {:?}", *gcs[i], gcs[i].as_ptr());
    }

    for i in 0..10 {
        #[repr(C)]
        #[derive(Debug, Clone)]
        struct ReturnedStruct {
            a: i32,
            /// OK, technically it's bad to have this be a static lifetime
            ///
            /// Since this is stored on the heap, we don't need it to be a `Gc<i32>`.
            /// `Gc<i32>` is only for rooted types, not heap types
            b: &'static i32,
        }

        let ret_addr = ret_ref_struct(i);
        let ret_ptr = unsafe { Gc::new(ret_addr as *mut ReturnedStruct) };
        println!("{i} -> {:?} at {:?}", &*ret_ptr, ret_ptr.as_ptr());
    }
}

pub fn main() {
    emscript_main::runtime::RuntimeBuilder::log_settings(false);
    emscript_main::runtime::testing::test_runtime_gc();

    // run_example_mul2add1();
    // run_example_fib();
    // run_example_refs();
    // emscript_main::run();
}
