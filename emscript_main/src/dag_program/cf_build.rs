use crate::runtime::{self, RuntimeBuilder, builtin};

use super::*;
use std::collections::HashMap;

use cranelift::{
    codegen::ir::{Function, StackSlot, StackSlotKey, condcodes},
    prelude::*,
};
use cranelift_jit::JITModule;
use cranelift_module::Module;
use itertools::Itertools;
use slotmap::{Key, SecondaryMap};

pub(crate) const CALL_CONV: isa::CallConv = isa::CallConv::SystemV;
pub(crate) const PTR_TYPE: types::Type = types::I64;

#[track_caller]
fn translate_ssa_type(program: &super::Program, ty: super::TypeId) -> types::Type {
    match program.types.get_info(ty).unwrap() {
        super::TypeInfo::PrimI32 => types::I32,
        super::TypeInfo::PrimI64 => types::I64,
        super::TypeInfo::PrimF32 => types::F32,
        super::TypeInfo::PrimBool => types::I8,
        super::TypeInfo::Ref(_) => PTR_TYPE,
        super::TypeInfo::GcRef(_) => PTR_TYPE,
        super::TypeInfo::Struct(ty) => {
            types::Type::int_with_byte_size(ty.layout(program).size as u16).unwrap()
        }
    }
}

/// * `root_ptr`: A CF variable pointing to the root struct of this place
fn cf_get_pointer_to_place_from_root_ptr(
    builder: &mut FunctionBuilder,
    program: &super::Program,
    place: &PlaceExpr,
    root_ptr: Value,
) -> Value {
    let mut curr_ptr = root_ptr;
    let mut curr_type = place.local_ty;

    for proj in &place.projs {
        let next_ptr: Value;
        let next_type: TypeId;
        match proj {
            Projection::Deref => {
                // let ptr: usize = 10;
                // let derefed_ptr: usize = MEMORY[ptr]; // The `usize` at the index `ptr`

                next_ptr = builder.ins().load(PTR_TYPE, MemFlags::new(), curr_ptr, 0);
                next_type = match program.types.get_info(curr_type).unwrap() {
                    TypeInfo::Ref(type_id) => type_id,
                    TypeInfo::GcRef(type_id) => type_id,
                    _ => panic!("Cannot dereference non-reference type!"),
                };
            }
            Projection::Field { field_idx } => {
                let struct_info = match program.types.get_info(curr_type).unwrap() {
                    TypeInfo::Struct(struct_info) => struct_info,
                    _ => panic!("Cannot do field operation on non-struct type!"),
                };
                // This optimization isn't needed, but makes code less cluttered
                next_ptr = match struct_info.field_offset(program, *field_idx) as i64 {
                    0 => curr_ptr,
                    field_offset => builder.ins().iadd_imm(curr_ptr, field_offset),
                };
                next_type = struct_info.fields[*field_idx];
            }
            Projection::Index { .. } => todo!(),
        }
        curr_ptr = next_ptr;
        curr_type = next_type;
    }

    curr_ptr
}

fn cf_get_pointer_to_place_from_stackslot(
    builder: &mut FunctionBuilder,
    program: &super::Program,
    place: &PlaceExpr,
    root_slot: StackSlot,
) -> Value {
    let root_addr = builder.ins().stack_addr(PTR_TYPE, root_slot, 0);
    cf_get_pointer_to_place_from_root_ptr(builder, program, place, root_addr)
}

fn cf_read_operand_value(
    builder: &mut FunctionBuilder,
    program: &super::Program,
    operand: &Operand,
    func: &Func,
    local_to_stack_slot: &HashMap<LocalId, StackSlot>,
) -> Value {
    match operand {
        Operand::Place(place_expr) => {
            let p: Value = cf_get_pointer_to_place_from_stackslot(
                &mut *builder,
                program,
                place_expr,
                local_to_stack_slot[&place_expr.local],
            );
            builder.ins().load(
                translate_ssa_type(program, place_expr.get_final_type(program, func)),
                MemFlags::new(),
                p,
                0,
            )
        }
        Operand::ConstI32(imm) => builder.ins().iconst(types::I32, *imm as i64),
        Operand::ConstF32(imm) => builder.ins().f32const(*imm),
        Operand::ConstBool(imm) => builder.ins().iconst(types::I8, if *imm { 1 } else { 0 }),
    }
}

struct BuiltinIds {
    fn_alloc: cranelift_module::FuncId,
    fn_push_stack_frame: cranelift_module::FuncId,
    fn_pop_stack_frame: cranelift_module::FuncId,
    data_runtime_ptr: cranelift_module::DataId,
}

struct FunctionCompRes {
    func: Function,
    gc_root_stackslots: Vec<(StackSlot, TypeId)>,
}

fn function_to_cranelift(
    program: &super::Program,
    func_id: super::FuncId,
    func_id_map: &HashMap<super::FuncId, cranelift_module::FuncId>,
    runtime_func_id_map: &SecondaryMap<super::FuncId, runtime::RtFuncId>,
    runtime_type_id_map: &SecondaryMap<super::TypeId, runtime::RtTypeId>,
    module: &mut impl Module,
    builtin_ids: &BuiltinIds,
) -> FunctionCompRes {
    let func = &program.functions[func_id];
    let mut sig = Signature::new(CALL_CONV);

    for local in &func.args {
        let arg_ty = func.locals[*local];
        let cf_ty = translate_ssa_type(program, arg_ty);
        sig.params.push(AbiParam::new(cf_ty));
    }

    if let Some(arg_ty) = func.return_type {
        // `runtime_pointer, return_value`
        let cf_ty = translate_ssa_type(program, arg_ty);
        sig.returns.push(AbiParam::new(cf_ty));
    }

    let mut fn_builder_ctx = FunctionBuilderContext::new();
    let mut cf_func = Function::with_name_signature(
        codegen::ir::UserFuncName::User(codegen::ir::UserExternalName {
            namespace: 0,
            // The lower half of the keydata is the index
            index: func_id.0.as_ffi() as u32,
        }),
        sig.clone(),
    );
    let func_refs: HashMap<_, codegen::ir::FuncRef> = program
        .functions
        .iter()
        .map(|(func_id, _)| {
            (
                func_id,
                module.declare_func_in_func(func_id_map[&func_id], &mut cf_func),
            )
        })
        .collect();
    let builtin_fn_alloc = module.declare_func_in_func(builtin_ids.fn_alloc, &mut cf_func);
    let builtin_fn_push_stack_frame =
        module.declare_func_in_func(builtin_ids.fn_push_stack_frame, &mut cf_func);
    let builtin_fn_pop_stack_frame =
        module.declare_func_in_func(builtin_ids.fn_pop_stack_frame, &mut cf_func);
    // let builtin_data_runtime_ptr =
    //     module.declare_data_in_func(builtin_ids.data_runtime_ptr, &mut cf_func);

    let mut builder = FunctionBuilder::new(&mut cf_func, &mut fn_builder_ctx);

    let mut gc_root_stackslots = vec![];
    let local_to_stack_slot: HashMap<LocalId, StackSlot> = func
        .locals
        .iter()
        .map(|(id, ty)| {
            let layout = program.types.get_info(*ty).unwrap().layout(program);
            let sskey = StackSlotKey::new(id.0.as_ffi() as u32 as u64);
            let ss = builder.create_sized_stack_slot(StackSlotData::new_with_key(
                StackSlotKind::ExplicitSlot,
                layout.size() as u32,
                layout.align().ilog2().try_into().unwrap(),
                sskey,
            ));
            if let TypeInfo::GcRef(_) = program.types.get_info(*ty).unwrap() {
                gc_root_stackslots.push((ss, func.locals[id]));
            }
            (id, ss)
            // builder.declare_var_needs_stack_map(ss);
        })
        .collect();

    let block_map = {
        let mut map = HashMap::new();
        map.insert(func.entrypoint, builder.create_block());
        for (block, _) in &func.blocks {
            if block == func.entrypoint {
                continue;
            }
            map.insert(block, builder.create_block());
        }
        map
    };

    // Setup block instructions
    for (block_id, block) in func.blocks.iter().sorted_by_key(|(bb, _)| {
        // We want to insert the entrypoint block first
        if *bb == func.entrypoint { 0 } else { 1 }
    }) {
        builder.switch_to_block(block_map[&block_id]);

        if block_id == func.entrypoint {
            // Setup entry block
            let entry_block = block_map[&func.entrypoint];
            // builder.switch_to_block(entry_block);
            builder.append_block_params_for_function_params(entry_block);
            for (arg_local, arg_value) in func
                .args
                .iter()
                .zip_eq(builder.block_params(entry_block).to_vec())
            {
                let ss = local_to_stack_slot[arg_local];
                builder.ins().stack_store(arg_value, ss, 0);
            }

            // Notify runtime of stack frame
            {
                let func_id_runtime = runtime_func_id_map[func_id];
                let func_id = builder
                    .ins()
                    .iconst(types::I64, func_id_runtime.data().as_ffi() as i64);
                let fp = builder.ins().get_frame_pointer(types::I64);

                builder
                    .ins()
                    .call(builtin_fn_push_stack_frame, &[func_id, fp]);
            }
        }

        for ins in &block.instructions {
            match ins {
                super::Ins::Assign {
                    place: lvalue,
                    value: rvalue,
                } => {
                    let lhs_root_slot = local_to_stack_slot[&lvalue.local];
                    let lhs_ptr: Value = cf_get_pointer_to_place_from_stackslot(
                        &mut builder,
                        program,
                        lvalue,
                        lhs_root_slot,
                    );

                    let rhs_value = match rvalue {
                        // We perform a copy
                        ValueExpr::Read(operand) => cf_read_operand_value(
                            &mut builder,
                            program,
                            operand,
                            func,
                            &local_to_stack_slot,
                        ),
                        // We keep the pointer the same
                        ValueExpr::Ref(operand) => match operand {
                            Operand::Place(place_expr) => {
                                let p: Value = cf_get_pointer_to_place_from_stackslot(
                                    &mut builder,
                                    program,
                                    place_expr,
                                    local_to_stack_slot[&place_expr.local],
                                );
                                p
                            }
                            Operand::ConstI32(_) | Operand::ConstF32(_) | Operand::ConstBool(_) => {
                                unreachable!(
                                    "Cannot reference immediate constant, fix by adding an intermediate variable"
                                )
                            }
                        },
                        ValueExpr::BinOp(bin_op, lhs_op, rhs_op) => {
                            let lhs = cf_read_operand_value(
                                &mut builder,
                                program,
                                lhs_op,
                                func,
                                &local_to_stack_slot,
                            );
                            let rhs = cf_read_operand_value(
                                &mut builder,
                                program,
                                rhs_op,
                                func,
                                &local_to_stack_slot,
                            );

                            let input_ty = program
                                .types
                                .get_info(lhs_op.get_final_type(program, &func))
                                .unwrap();

                            let output_ty = program
                                .types
                                .get_info(lvalue.get_final_type(program, &func))
                                .unwrap();

                            match (bin_op, &output_ty) {
                                // A `true` is any value other than 0
                                (BinOp::Eq, TypeInfo::PrimBool) => match input_ty {
                                    TypeInfo::PrimI32 | TypeInfo::PrimBool => {
                                        builder.ins().icmp(condcodes::IntCC::Equal, lhs, rhs)
                                    }
                                    TypeInfo::PrimF32 => {
                                        builder.ins().fcmp(condcodes::FloatCC::Equal, lhs, rhs)
                                    }
                                    _ => panic!(),
                                },
                                (BinOp::Le, TypeInfo::PrimBool) => match input_ty {
                                    TypeInfo::PrimI32 => builder.ins().icmp(
                                        condcodes::IntCC::SignedLessThanOrEqual,
                                        lhs,
                                        rhs,
                                    ),
                                    TypeInfo::PrimF32 => builder.ins().fcmp(
                                        condcodes::FloatCC::LessThanOrEqual,
                                        lhs,
                                        rhs,
                                    ),
                                    _ => panic!(),
                                },
                                (BinOp::Lt, TypeInfo::PrimBool) => match input_ty {
                                    TypeInfo::PrimI32 => builder.ins().icmp(
                                        condcodes::IntCC::SignedLessThan,
                                        lhs,
                                        rhs,
                                    ),
                                    TypeInfo::PrimF32 => {
                                        builder.ins().fcmp(condcodes::FloatCC::LessThan, lhs, rhs)
                                    }
                                    _ => panic!(),
                                },

                                (BinOp::Add, TypeInfo::PrimI32) => builder.ins().iadd(lhs, rhs),
                                (BinOp::Add, TypeInfo::PrimF32) => builder.ins().fadd(lhs, rhs),

                                (BinOp::Sub, TypeInfo::PrimI32) => builder.ins().isub(lhs, rhs),
                                (BinOp::Sub, TypeInfo::PrimF32) => builder.ins().fsub(lhs, rhs),

                                (BinOp::Mul, TypeInfo::PrimI32) => builder.ins().imul(lhs, rhs),
                                (BinOp::Mul, TypeInfo::PrimF32) => builder.ins().fmul(lhs, rhs),

                                (BinOp::Div, TypeInfo::PrimI32) => builder.ins().sdiv(lhs, rhs),
                                (BinOp::Div, TypeInfo::PrimF32) => builder.ins().fdiv(lhs, rhs),
                                _ => panic!(
                                    "Bad instruction: (bin_op, output_ty) = ({:?}, {:?})",
                                    bin_op, output_ty
                                ),
                            }
                        }
                    };

                    // builder.ins().store(MemFlags::new(), rhs_value, lhs_ptr, 0);
                    builder.ins().store(MemFlags::new(), rhs_value, lhs_ptr, 0);
                }
                super::Ins::AllocGc { place } => {
                    let lvalue = cf_get_pointer_to_place_from_stackslot(
                        &mut builder,
                        program,
                        place,
                        local_to_stack_slot[&place.local],
                    );
                    let place_final_ty = place.get_final_type(program, func);
                    let TypeInfo::GcRef(pointee_type) =
                        program.types.get_info(place_final_ty).unwrap()
                    else {
                        panic!();
                    };

                    let pointee_runtime_ty = runtime_type_id_map[pointee_type];

                    let ty = builder
                        .ins()
                        .iconst(types::I64, pointee_runtime_ty.data().as_ffi() as i64);
                    let alloc_call_ins = builder.ins().call(builtin_fn_alloc, &[ty]);
                    let rets = builder.inst_results(alloc_call_ins).to_vec();
                    builder.ins().store(MemFlags::new(), rets[0], lvalue, 0);
                }
            }
        }

        // Leave block!
        match &block.ins_final {
            InsFinal::Br { next } => {
                builder.ins().jump(block_map[next], []);
            }
            InsFinal::BrIf {
                cond,
                is_false,
                is_true,
            } => {
                assert_eq!(translate_ssa_type(program, func.locals[*cond]), types::I8);
                let cond = builder
                    .ins()
                    .stack_load(types::I8, local_to_stack_slot[cond], 0);
                builder
                    .ins()
                    .brif(cond, block_map[is_true], [], block_map[is_false], []);
            }
            InsFinal::Call {
                func: func_called,
                args,
                store_ret,
                next,
            } => {
                let args = args
                    .iter()
                    .map(|arg| {
                        builder.ins().stack_load(
                            translate_ssa_type(program, func.locals[*arg]),
                            local_to_stack_slot[arg],
                            0,
                        )
                    })
                    .collect_vec();
                let rets = builder.ins().call(func_refs[&func_called], &args);
                let rets = builder.inst_results(rets).to_vec();
                assert!(rets.len() < 2);
                if let Some(store_ret) = store_ret {
                    builder
                        .ins()
                        .stack_store(rets[0], local_to_stack_slot[store_ret], 0);
                }

                builder.ins().jump(block_map[next], []);
            }
            InsFinal::Return { value } => {
                // Notify runtime of returning
                {
                    builder.ins().call(builtin_fn_pop_stack_frame, &[]);
                }
                match value {
                    Some(value) => {
                        let value: Value = builder.ins().stack_load(
                            sig.returns[0].value_type,
                            local_to_stack_slot[&value],
                            0,
                        );
                        builder.ins().return_(&[value]);
                    }
                    None => {
                        builder.ins().return_(&[]);
                    }
                }
            }
        }
    }
    builder.seal_all_blocks();

    FunctionCompRes {
        func: cf_func,
        gc_root_stackslots,
    }
}

pub struct CompiledProgram {
    pub module: JITModule,
    pub func_name_to_id: HashMap<String, cranelift_module::FuncId>,
}

impl CompiledProgram {
    /// Return type must be `extern "sysv64" fn(..) -> ..` with the correct arguments
    pub unsafe fn get_function<T>(&self, func_name: &str) -> T {
        let func_raw_ptr = self
            .module
            .get_finalized_function(self.func_name_to_id[func_name]);
        unsafe { std::mem::transmute_copy(&func_raw_ptr) }
    }
}

pub fn compile_program(program: &Program) -> CompiledProgram {
    let mut jit_builder = cranelift_jit::JITBuilder::with_flags(
        &[("preserve_frame_pointers", "true")],
        Box::new(cranelift_module::default_libcall_names()),
    )
    .unwrap();
    jit_builder.symbol(builtin::FN_ALLOCGC.name, builtin::FN_ALLOCGC.func);
    jit_builder.symbol(
        builtin::FN_PUSHSTACKFRAME.name,
        builtin::FN_PUSHSTACKFRAME.func,
    );
    jit_builder.symbol(
        builtin::FN_POPSTACKFRAME.name,
        builtin::FN_POPSTACKFRAME.func,
    );

    let mut func_to_rt_func = SecondaryMap::new();
    let mut ty_to_rt_ty = SecondaryMap::new();

    for (ty_id, ty_info) in program.types.all_types() {
        let gc_ptr_fields = match &ty_info {
            TypeInfo::PrimI32
            | TypeInfo::PrimI64
            | TypeInfo::PrimF32
            | TypeInfo::PrimBool
            | TypeInfo::Ref(_) => vec![],
            TypeInfo::GcRef(_) => vec![0],
            TypeInfo::Struct(struct_info) => struct_info
                .layout(program)
                .field_offsets
                .into_iter()
                .zip_eq(struct_info.fields.clone())
                .filter_map(|(offset, ty)| {
                    if let TypeInfo::GcRef(_) = program.types.get_info(ty).unwrap() {
                        Some(offset)
                    } else {
                        None
                    }
                })
                .collect_vec(),
        };
        ty_to_rt_ty.insert(
            ty_id,
            RuntimeBuilder::declare_define_type(runtime::RtTypeData {
                layout: ty_info.layout(program),
                gc_ptr_fields,
            }),
        );
    }

    let mut module = cranelift_jit::JITModule::new(jit_builder);
    let mut func_id_map: HashMap<FuncId, cranelift_module::FuncId> = HashMap::new();
    for (func_id, func) in &program.functions {
        let mut sig = Signature::new(CALL_CONV);
        if let Some(ret) = func.return_type {
            sig.returns
                .push(AbiParam::new(translate_ssa_type(&program, ret)));
        }
        sig.params.extend(
            func.args
                .iter()
                .map(|x| AbiParam::new(translate_ssa_type(&program, func.locals[*x]))),
        );

        let cf_func_id = module
            .declare_function(&func.name, cranelift_module::Linkage::Export, &sig)
            .unwrap();
        func_id_map.insert(func_id, cf_func_id);
        func_to_rt_func.insert(func_id, RuntimeBuilder::declare_func());
    }

    let builtin_ids = BuiltinIds {
        fn_alloc: module
            .declare_function(
                builtin::FN_ALLOCGC.name,
                cranelift_module::Linkage::Import,
                &builtin::FN_ALLOCGC.sig(),
            )
            .unwrap(),
        fn_push_stack_frame: module
            .declare_function(
                builtin::FN_PUSHSTACKFRAME.name,
                cranelift_module::Linkage::Import,
                &builtin::FN_PUSHSTACKFRAME.sig(),
            )
            .unwrap(),
        fn_pop_stack_frame: module
            .declare_function(
                builtin::FN_POPSTACKFRAME.name,
                cranelift_module::Linkage::Import,
                &builtin::FN_POPSTACKFRAME.sig(),
            )
            .unwrap(),
        data_runtime_ptr: module
            .declare_data(
                builtin::RT_PTR_SYM,
                cranelift_module::Linkage::Import,
                false,
                false,
            )
            .unwrap(),
    };

    // Definitions
    for (func_id, _) in &program.functions {
        let func = function_to_cranelift(
            &program,
            func_id,
            &func_id_map,
            &func_to_rt_func,
            &ty_to_rt_ty,
            &mut module,
            &builtin_ids,
        );

        let FunctionCompRes {
            func,
            gc_root_stackslots,
        } = func;

        println!("Generated Func:\n{}", func.display());

        let mut codegen_ctx = codegen::Context::for_function(func);

        if let Err(e) = module.define_function(func_id_map[&func_id], &mut codegen_ctx) {
            panic!("`{e:#?}`");
        }

        // println!("Optimized:\n{}", codegen_ctx.func.display());

        let compiled = codegen_ctx.compiled_code().unwrap();
        dbg!(compiled.buffer.user_stack_maps());

        let mut rt_funcdata = runtime::RtFuncData { roots: vec![] };

        for (gc_root_ss, _heap_ty) in gc_root_stackslots {
            let frame_layout = compiled.buffer.frame_layout().unwrap();
            let root_offset = frame_layout.stackslots.get(gc_root_ss).unwrap().offset;
            rt_funcdata.roots.push(root_offset as usize);
        }

        RuntimeBuilder::define_func(func_to_rt_func[func_id], rt_funcdata);

        module.clear_context(&mut codegen_ctx);
    }

    module.finalize_definitions().unwrap();

    CompiledProgram {
        module,
        func_name_to_id: program
            .functions
            .iter()
            .map(|(p_id, func)| (func.name.clone(), func_id_map[&p_id]))
            .collect(),
    }
}
