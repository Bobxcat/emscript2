use std::{
    alloc::Layout,
    collections::{HashMap, HashSet, hash_map::Entry},
    ops::Deref,
    ptr::NonNull,
    sync::{LazyLock, Mutex, RwLock},
    time::{Duration, Instant},
};

use itertools::Itertools;
use slotmap::{SecondaryMap, SlotMap};

use crate::dag_program::{FuncId, TypeId};

// Create runtime
// Populate with types and functions
// Leak to heap
// Set CF global value to runtime pointer

// /// While the CF program is running, the Runtime instance must not be accessed
// pub(crate) fn create_cf_runtime_instance() -> NonNull<Runtime> {
//     Box::leak(Box::new(Runtime {})).into()
// }

slotmap::new_key_type! {
    pub struct RtTypeId;
    pub struct RtFuncId;
}

#[derive(Debug, Clone)]
pub struct RtTypeData {
    pub layout: Layout,
    pub gc_ptr_fields: Vec<usize>,
}

pub struct RtFuncData {
    /// Negative offsets from the frame pointer (the stack grows down)
    pub roots: Vec<usize>,
}

/// Used to give program layout information to the runtime before running it
///
/// All functions are free
pub struct RuntimeBuilder;

impl RuntimeBuilder {
    pub fn log_settings(enable: bool) {
        Runtime::with_instance(|rt| {
            rt.log_enabled = enable;
        })
    }

    pub(crate) fn declare_func() -> RtFuncId {
        Self::declare_funcs(1)[0]
    }

    pub(crate) fn declare_funcs(count: usize) -> Vec<RtFuncId> {
        Runtime::with_instance(|rt| {
            (0..count)
                .map(|_| rt.funcs.insert(RtFuncData { roots: vec![] }))
                .collect_vec()
        })
    }

    pub(crate) fn define_func(func: RtFuncId, data: RtFuncData) {
        Runtime::with_instance(|rt| {
            rt.funcs[func] = data;
        })
    }

    pub(crate) fn declare_define_type(ty: RtTypeData) -> RtTypeId {
        Self::declare_define_types([ty])[0]
    }

    pub(crate) fn declare_define_types(
        types: impl IntoIterator<Item = RtTypeData>,
    ) -> Vec<RtTypeId> {
        Runtime::with_instance(|rt| {
            types
                .into_iter()
                .map(|data| rt.types.insert(data))
                .collect_vec()
        })
    }
}

struct RtStackFrame {
    roots: Vec<*const *const u8>,
}

pub mod testing {
    use std::{
        alloc::Layout,
        marker::PhantomData,
        mem::offset_of,
        ops::{Deref, DerefMut},
    };

    use crate::runtime::{RtTypeData, Runtime};

    unsafe trait GcAble {
        fn type_data() -> RtTypeData;
    }

    unsafe trait IntoRooted {
        type RootedVer;
        /// Tell the GC that this is now on the stack
        unsafe fn root(self, rt: &mut Runtime) -> Self::RootedVer;
    }
    unsafe trait IntoNonRooted {
        type NonRootedVer;
        /// Tell the GC that this isn't on the stack anymore
        unsafe fn unroot(self, rt: &mut Runtime) -> Self::NonRootedVer;
    }

    #[repr(transparent)]
    struct Rooted<T> {
        ptr: *const T,
    }

    impl<T> Rooted<T> {
        pub fn null() -> Self {
            Self {
                ptr: std::ptr::null(),
            }
        }

        /// Puts a value onto the heap
        pub fn new<U>(value: U) -> Self
        where
            U: IntoNonRooted<NonRootedVer = T>,
            T: GcAble,
        {
            // We unroot everything inside of `value`, but
            // we root our new reference to `value`
            Runtime::with_instance(|rt| {
                let value_non_rooted = unsafe { value.unroot(rt) };

                let ptr = rt.alloc_gc(T::type_data()).cast::<T>();
                unsafe { ptr.cast_mut().write(value_non_rooted) };
                rt.refcount_increment(ptr);

                Self { ptr }
            })
        }
    }

    impl<T> Clone for Rooted<T> {
        fn clone(&self) -> Self {
            Runtime::with_instance(|rt| {
                rt.refcount_increment(self.ptr);
            });

            Self { ptr: self.ptr }
        }
    }

    impl<T> Drop for Rooted<T> {
        fn drop(&mut self) {
            Runtime::with_instance(|rt| {
                rt.refcount_decrement(self.ptr);
            })
        }
    }

    impl<T> Deref for Rooted<T> {
        type Target = T;

        fn deref(&self) -> &Self::Target {
            unsafe { &*self.ptr }
        }
    }

    impl<T> DerefMut for Rooted<T> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            unsafe { &mut *self.ptr.cast_mut() }
        }
    }

    /// You can't directly create or clone a `NonRooted<T>`!
    #[repr(transparent)]
    struct NonRooted<T> {
        ptr: *const T,
    }

    impl<T> NonRooted<T> {
        pub fn as_rooted(&self) -> Rooted<T> {
            // What we're pointing to will remain unrooted, but we're gaining a root
            // so we need to account for that
            Runtime::with_instance(|rt| {
                rt.refcount_increment(self.ptr);
                Rooted { ptr: self.ptr }
            })
        }

        pub fn set(&mut self, to_ptr: &Rooted<T>) {
            // I think this is fine, since it doesn't let you grab a `NonRooted<T>` outside of Rooted access
            self.ptr = to_ptr.ptr;
        }
    }

    impl<T> Deref for NonRooted<T> {
        type Target = T;

        fn deref(&self) -> &Self::Target {
            unsafe { &*self.ptr }
        }
    }

    impl<T> DerefMut for NonRooted<T> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            unsafe { &mut *self.ptr.cast_mut() }
        }
    }

    unsafe impl<T> IntoNonRooted for Rooted<T> {
        type NonRootedVer = NonRooted<T>;

        unsafe fn unroot(self, rt: &mut Runtime) -> Self::NonRootedVer {
            rt.refcount_decrement(self.ptr);
            let non_rooted = NonRooted { ptr: self.ptr };
            // Don't drop self!!! That locks the runtime so we deal with updating the refcount separately
            std::mem::forget(self);
            non_rooted
        }
    }

    /// Lives on stack
    #[repr(C)]
    struct LinkedList {
        val: i32,
        next: Rooted<LinkedListNonRooted>,
    }

    impl std::fmt::Debug for LinkedList {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("LinkedList")
                .field("val", &self.val)
                .field("next", &*self.next)
                .finish()
        }
    }

    unsafe impl IntoNonRooted for LinkedList {
        type NonRootedVer = LinkedListNonRooted;

        unsafe fn unroot(self, rt: &mut Runtime) -> Self::NonRootedVer {
            LinkedListNonRooted {
                val: self.val,
                next: unsafe { self.next.unroot(rt) },
            }
        }
    }

    /// Lives on heap
    #[repr(C)]
    struct LinkedListNonRooted {
        val: i32,
        next: NonRooted<LinkedListNonRooted>,
    }

    impl std::fmt::Debug for LinkedListNonRooted {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("LinkedListNonRooted")
                .field("val", &self.val)
                .field("next", &*self.next)
                .finish()
        }
    }

    unsafe impl GcAble for LinkedListNonRooted {
        fn type_data() -> RtTypeData {
            RtTypeData {
                layout: Layout::new::<Self>(),
                gc_ptr_fields: vec![offset_of!(Self, next)],
            }
        }
    }

    unsafe impl IntoRooted for LinkedListNonRooted {
        type RootedVer = LinkedList;

        unsafe fn root(self, rt: &mut Runtime) -> Self::RootedVer {
            todo!()
        }
    }

    pub fn test_runtime_gc() {
        println!("START");
        let mut elem2 = Rooted::new(LinkedList {
            val: 2,
            next: Rooted::null(),
        });
        let mut elem1 = Rooted::new(LinkedList {
            val: 1,
            next: elem2.clone(),
        });
        elem2.next.set(&elem1);
        let elem0 = Rooted::new(LinkedList {
            val: 0,
            next: elem1.clone(),
        });

        let a = &elem2.val;
        let a_aliasing = &mut elem1.next.val;
        println!("a={a}");
        *a_aliasing += 1;
        println!("a_aliased={a_aliasing},a={a}");

        println!("v0={}", elem0.val);
        println!("v1={}", elem1.val);
        println!("v2={}", elem2.val);

        // let s = format!("{:?}", &*elem0);
        // let x = elem
    }
}

/// # Notes
/// * It should be *impossible* for a user to write Emscript code which generates unsound behavior
/// * For every `Gc<T>`-able struct, we should generate two layout-identical structs,
/// a rooted and non-rooted version. The rooted version has all references as `Gc<T>` values, and is used
/// when the struct is read from a `Gc<T>` pointer and put on the stack. The non-rooted version of the struct should instead
/// use normal references (should we have a `NonRooted<T>` instead?),
/// and their lifetime is obtained by the dereference of the `Gc<T>` that points to it
/// * As long as no Emscript is running, accessing rules for a garbage collected value are fundamentally
/// doable via a reentrant `RwLock`. A reentrant `Mutex` could work as well
/// * When the embedder dereferences a garbage collected reference, we wait for Emscript to enter a safepoint and halt it until all
/// Gc locks are freed. This kinda sucks though, maybe the best option is to lock all GC values even in the Emscript code?
/// * It's possible that using a `with_ref` and `with_mut` api could circumvent this a little (the functions taking the argument: `FnOnce(&Pointee)`)
///
/// ```
/// /// A LinkedList living on the stack
/// #[repr(C)]
/// struct LinkedList {
///     val: i32,
///     next: Gc<LinkedList>,
/// }
///
/// /// A LinkedList living on the heap
/// #[repr(C)]
/// struct LinkedListNonRooted<'root> {
///     val: i32,
///     next: NonRooted<LinkedListNonRooted>,
/// }
///
/// let root: Gc<LinkedList> = emscript_function_call();
/// let next_non_rooted: &LinkedList = &*root.next; // Lifetime tied to `root` above
/// let next2_non_rooted: &LinkedList = &*next_non_rooted.next; // Lifetime tied to `root` above
/// let next_rooted = root.next.clone(); // Rooted, no lifetime
/// let next2_rooted = next_non_rooted.next.rooted(); // Promotion from NonRooted to Rooted, no lifetime
/// ```
/// * How do we avoid having a `NonRooted<T>` on the stack? How do we avoid having a `Gc<T>` on the heap?
/// Is there a way for `Gc<T>` to automatically unroot itself when it's moved to the heap (no, because moves aren't trackable)?
/// This needs very careful type and trait design.
///
///
/// * In order to support multi-threading, we need a way to wait for all threads
/// to enter a safepoint, which shouldn't create a deadlock (i.e. we're waiting for safepoint
/// on thread #0 and thread #1 calls `enter_safepoint` which tries to lock the Runtime and
/// is unhappy)
/// * We need to think about how to handle mutability of `Gc<T>`. You shouldn't be able to hold a `&T`
/// obtained from dereferencing `Gc<T>` across any Emscript running. However, requiring a lock for every
/// `Gc<T>` access by the Emscript and Embedder seems like an awful idea. Should the runtime keep track of
/// when Emscript code is running and require `Gc<T>` accesses to wait for it to finish?
/// * The `Runtime` instance should probably be in charge of managing clients calling Emscript functions
struct Runtime {
    log_enabled: bool,
    stack: Vec<RtStackFrame>,
    funcs: SlotMap<RtFuncId, RtFuncData>,
    types: SlotMap<RtTypeId, RtTypeData>,
    last_sweep: Instant,
    heap: HashMap<*const u8, RtTypeData>,
    extern_refcount: HashMap<*const u8, usize>,
}

unsafe impl Send for Runtime {}
unsafe impl Sync for Runtime {}

impl std::fmt::Display for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Runtime(0x{:x})", 0)
    }
}

impl Runtime {
    fn with_instance<T>(f: impl FnOnce(&mut Runtime) -> T) -> T {
        static RUNTIME_INSTANCE: LazyLock<Mutex<Runtime>> = LazyLock::new(|| {
            Mutex::new(Runtime {
                log_enabled: true,
                stack: Default::default(),
                funcs: Default::default(),
                types: Default::default(),
                last_sweep: Instant::now(),
                heap: Default::default(),
                extern_refcount: Default::default(),
            })
        });

        let mut instance = RUNTIME_INSTANCE.lock().unwrap();
        f(&mut *instance)
    }

    fn refcount_increment<T>(&mut self, ptr: *const T) {
        let refct = self.extern_refcount.entry(ptr.cast()).or_insert(0);
        *refct += 1;
    }

    fn refcount_decrement<T>(&mut self, ptr: *const T) {
        let ptr = ptr.cast::<u8>();
        match self.extern_refcount.entry(ptr) {
            Entry::Occupied(mut o) => {
                let refct = o.get_mut();
                debug_assert!(*refct > 0);
                *refct -= 1;
                if *refct == 0 {
                    self.extern_refcount.remove(&ptr);
                    self.safepoint();
                }
            }
            Entry::Vacant(_) => {
                if self.log_enabled {
                    eprintln!("WARN: Gc<T> pointed to invalid memory!");
                }
            }
        }
    }

    fn alloc_gc(&mut self, ty: RtTypeData) -> *const u8 {
        let p = unsafe { std::alloc::alloc(ty.layout) };
        if p.is_null() {
            std::alloc::handle_alloc_error(ty.layout);
        }

        self.heap.insert(p, ty);

        p
    }

    fn safepoint(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_sweep) > Duration::from_millis(0) {
            self.last_sweep = now;
            self.sweep();
        }
    }

    fn sweep(&mut self) {
        if self.log_enabled {
            println!("::::Started GC sweep for rt={self}");
        }

        let reachable: HashSet<*const u8> = {
            let mut reachable: HashSet<*const u8> = HashSet::new();
            let mut visit_stack: Vec<*const u8> = vec![];

            for &ptr_to_root in self
                .stack
                .iter()
                .flat_map(|stack_frame| stack_frame.roots.iter())
            {
                // println!("{:?} -> {:?}", ptr_to_root, unsafe { ptr_to_root.read() });
                visit_stack.push(unsafe { ptr_to_root.read() });
            }

            {
                for (ptr, ct) in &self.extern_refcount {
                    debug_assert!(*ct > 0);
                    visit_stack.push(*ptr);
                }
            }

            if self.log_enabled {
                println!("::::::GC heap:\n`{:#?}`", self.heap);
                println!("::::::GC roots: `{visit_stack:?}`");
            }

            while let Some(base_ptr) = visit_stack.pop() {
                if base_ptr.is_null() {
                    continue;
                }
                if !reachable.insert(base_ptr) {
                    continue;
                }

                let ty = &self.heap[&base_ptr];
                for &field_offset in &ty.gc_ptr_fields {
                    // SAFETY: The allocation should contain this field, but this still might not be safe
                    let field_ptr = unsafe { base_ptr.add(field_offset).cast::<*const u8>() };
                    // SAFETY: Our heap values are well formed
                    let field_points_to: *const u8 = unsafe { field_ptr.read() };
                    visit_stack.push(field_points_to);
                }
            }
            reachable
        };

        self.heap.retain(|&heap_ptr, heap_ty| {
            if reachable.contains(&heap_ptr) {
                return true;
            }

            if self.log_enabled {
                println!("::::::Deallocating unreachable GC value: `{heap_ptr:?}`");
            }

            // SAFETY: Our pointer was allocated through this Runtime instance,
            // and the type information and allocation are both valid
            unsafe { std::alloc::dealloc(heap_ptr.cast_mut(), heap_ty.layout) };

            false
        });

        if self.log_enabled {
            println!("::::::Sweep finished");
        }
    }
}

/// A pointer to managed Emscript memory
///
/// This type is transmutable from an `i64` pointer returned by Cranelift *if you know what you're doing*
#[derive(Debug)]
#[repr(C)]
pub struct Gc<T> {
    data: *mut T,
}

impl<T: Clone> Clone for Gc<T> {
    fn clone(&self) -> Self {
        unsafe { Self::new(self.data) }
    }
}

impl<T> Gc<T> {
    /// # Safety
    /// * `data` must be correctly typed and point to a runtime-tracked value
    /// # TODO
    /// Use a trait to mark types as runtime-useable, which will
    /// register them in the Runtime type table. This trait should be able to check
    /// that `data` is *actually* of the type that we're saying it is, which makes
    /// this a fallible safe method instead of an unsafe one
    pub unsafe fn new(data: *mut T) -> Self {
        Runtime::with_instance(|rt| rt.refcount_increment(data.cast_const()));
        Self { data }
    }

    pub fn as_ptr(&self) -> *mut T {
        self.data
    }
}

impl<T> Deref for Gc<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.data }
    }
}
impl<T> Drop for Gc<T> {
    fn drop(&mut self) {
        Runtime::with_instance(|rt| rt.refcount_decrement(self.data.cast_const()));
        // Runtime::with_instance(|rt| {
        //     let data = self.data.cast_const().cast::<u8>();
        //     match rt.extern_refcount.entry(data) {
        //         Entry::Occupied(mut o) => {
        //             let refct = o.get_mut();
        //             debug_assert!(*refct > 0);
        //             *refct -= 1;
        //             if *refct == 0 {
        //                 rt.extern_refcount.remove(&data);
        //                 rt.safepoint();
        //             }
        //         }
        //         Entry::Vacant(_) => {
        //             eprintln!("WARN: Gc<T> pointed to invalid memory!");
        //         }
        //     }
        // });
    }
}
// impl<T> Gc<T> {
//     /// `data` and `runtime` must be valid and mutable
//     pub unsafe fn new(data: *mut T, runtime: *mut u8) -> Self {
//         unsafe {
//             let shared = &raw mut (*runtime.cast::<Runtime>()).shared;
//             let shared = &mut *shared;
//             let mut shared = shared.lock().unwrap();
//             let refcount = shared
//                 .extern_refcount
//                 .entry(data.cast_const().cast::<u8>())
//                 .or_insert(0);
//             *refcount += 1;
//         }
//         Self {
//             data,
//         }
//     }
// }

// impl<T> Drop for Gc<T> {
//     fn drop(&mut self) {
//         unsafe {
//             let shared = &raw mut (*self.runtime).shared;
//             let shared = &mut *shared;
//             let mut shared = shared.lock().unwrap();
//             let data_ptr = self.data.cast_const().cast::<u8>();
//             let refcount = shared.extern_refcount.entry(data_ptr).or_insert(0);
//             *refcount -= 1;
//             if *refcount == 0 {
//                 shared.extern_refcount.remove(&data_ptr);
//             }
//         }
//     }
// }

pub(crate) mod builtin {
    use cranelift::prelude::{AbiParam, Signature, Type, types};
    use itertools::Itertools;
    use slotmap::KeyData;

    use crate::{
        dag_program::cf_build::{CALL_CONV, PTR_TYPE},
        runtime::{RtFuncId, RtStackFrame, RtTypeId, Runtime},
    };

    pub const RT_PTR_SYM: &str = "builtin::runtime_ptr";

    pub struct BuiltinFn {
        pub name: &'static str,
        pub func: *const u8,
        params: &'static [Type],
        ret: Option<Type>,
    }

    impl BuiltinFn {
        pub fn sig(&self) -> Signature {
            let mut sig = Signature::new(CALL_CONV);
            sig.params
                .extend(self.params.iter().map(|t| AbiParam::new(*t)));
            if let Some(ret) = self.ret {
                sig.returns.push(AbiParam::new(ret));
            }
            sig
        }
    }

    pub const FN_PUSHSTACKFRAME: BuiltinFn = BuiltinFn {
        name: "builtin::push_stack_frame",
        func: push_stack_frame as *const u8,
        params: &[types::I64, types::I64],
        ret: None,
    };
    extern "sysv64" fn push_stack_frame(func_id: i64, frame_ptr: i64) {
        let func_id = RtFuncId::from(KeyData::from_ffi(func_id as u64));
        let frame_ptr = frame_ptr as *const u8;

        Runtime::with_instance(|rt| {
            if rt.log_enabled {
                println!(
                    "::::Called push_stack_frame with: func_id={func_id:?},frame_ptr={frame_ptr:?}"
                );
            }

            let func_data = &rt.funcs[func_id];
            let stack_frame_roots = func_data
                .roots
                .iter()
                .map(|&offset| {
                    // Our provenance is exposed, so I don't like using ptr::add and ptr::sub
                    (frame_ptr as usize - offset) as *const *const u8
                })
                .collect_vec();

            // We need to nullify all the root pointers in this stack frame so that the `safepoint` call is valid
            for &root in &stack_frame_roots {
                unsafe { root.cast_mut().write(std::ptr::null()) };
            }

            rt.stack.push(RtStackFrame {
                roots: stack_frame_roots,
            });

            rt.safepoint();
        })
    }

    pub const FN_POPSTACKFRAME: BuiltinFn = BuiltinFn {
        name: "builtin::pop_stack_frame",
        func: pop_stack_frame as *const u8,
        params: &[],
        ret: None,
    };
    extern "sysv64" fn pop_stack_frame() {
        Runtime::with_instance(|rt| {
            if rt.log_enabled {
                println!("::::Called pop_stack_frame");
            }
            rt.stack.pop();
            // This is NOT a safepoint since if this function returns a GC pointer,
            // that pointer isn't yet flushed to the stack (or to a Gc<T> class)
            // rt.safepoint();
        })
    }

    pub const FN_ALLOCGC: BuiltinFn = BuiltinFn {
        name: "builtin::alloc_gc",
        func: alloc_gc as *const u8,
        params: &[types::I64],
        ret: Some(PTR_TYPE),
    };
    extern "sysv64" fn alloc_gc(ty: i64) -> i64 {
        let ty = RtTypeId::from(KeyData::from_ffi(ty as u64));

        Runtime::with_instance(|rt| {
            if rt.log_enabled {
                println!("::::Called alloc_gc with: ty={ty:?}");
            }
            rt.alloc_gc(rt.types[ty].clone()) as i64
        })
    }
}

// struct GCRef

struct GC {
    //
}
