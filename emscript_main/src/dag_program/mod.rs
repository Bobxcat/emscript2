//! Mid level control-flow graph of a program
//!
//! 1. Build Program struct from text MIR or AST (it should be well formed)
//! 2. Promote references into GC if needed
//! 3.

pub mod cf_build;
pub mod human_format;
// pub mod interp;

use std::{
    alloc::Layout,
    collections::HashMap,
    sync::{Arc, Mutex},
};

use itertools::Itertools;
use slotmap::SlotMap;

slotmap::new_key_type! {
    pub struct TypeId;
    pub struct FuncId;
    pub struct BasicBlockId;

    pub struct LocalId;
    struct HeapRef;
}

#[derive(Debug, Clone)]
pub enum Projection {
    Deref,
    Field { field_idx: usize },
    Index { idx_local: LocalId },
}

/// An LValue
#[derive(Debug, Clone)]
pub struct PlaceExpr {
    pub local_ty: TypeId,
    /// The base local to project from
    pub local: LocalId,
    /// The projections, evaluated from left to right
    /// `my_local[1].field0` would have two projections, in the order
    /// `[Index, Field]`
    pub projs: Vec<Projection>,
}

impl PlaceExpr {
    pub fn get_final_type(&self, program: &Program, func: &Func) -> TypeId {
        let mut curr = self.local_ty;
        for proj in &self.projs {
            let curr_info = program.types.get_info(curr).unwrap();
            let next;
            match proj {
                Projection::Deref {} => match curr_info {
                    TypeInfo::Ref(type_id) => next = type_id,
                    TypeInfo::GcRef(type_id) => next = type_id,
                    _ => unreachable!(),
                },
                Projection::Field { field_idx } => match curr_info {
                    TypeInfo::Struct(struct_info) => next = struct_info.fields[*field_idx],
                    _ => unreachable!(),
                },
                Projection::Index { idx_local: _ } => todo!(),
            }
            curr = next;
        }
        curr
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,

    Eq,
    Lt,
    Le,
}

#[derive(Debug, Clone)]
pub enum Operand {
    Place(PlaceExpr),
    ConstI32(i32),
    ConstF32(f32),
    ConstBool(bool),
}

impl Operand {
    pub fn get_final_type(&self, program: &Program, func: &Func) -> TypeId {
        match self {
            Operand::Place(place_expr) => place_expr.get_final_type(program, func),
            Operand::ConstI32(_) => program.types.i32(),
            Operand::ConstF32(_) => program.types.f32(),
            Operand::ConstBool(_) => program.types.bool(),
        }
    }
}

/// An RValue
#[derive(Debug, Clone)]
pub enum ValueExpr {
    Read(Operand),
    Ref(Operand),
    /// Performs a binary operation on a primitive type
    BinOp(BinOp, Operand, Operand),
}

impl ValueExpr {
    pub fn get_final_type(&self, program: &Program, func: &Func) -> TypeId {
        match self {
            ValueExpr::Read(place_expr) => place_expr.get_final_type(program, func),
            ValueExpr::Ref(place_expr) => program
                .types
                .get_id(&TypeInfo::Ref(place_expr.get_final_type(program, func))),
            ValueExpr::BinOp(_, place_expr, _) => place_expr.get_final_type(program, func),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Ins {
    Assign { place: PlaceExpr, value: ValueExpr },
    AllocGc { place: PlaceExpr },
}

#[derive(Debug, Clone)]
pub enum InsFinal {
    Br {
        next: BasicBlockId,
    },
    BrIf {
        /// Must be a bool
        cond: LocalId,
        is_true: BasicBlockId,
        is_false: BasicBlockId,
    },
    Call {
        func: FuncId,
        args: Vec<LocalId>,
        store_ret: Option<LocalId>,
        next: BasicBlockId,
    },
    Return {
        value: Option<LocalId>,
    },
}

pub struct StructLayout {
    pub field_offsets: Vec<usize>,
    pub size: usize,
    pub align: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StructInfo {
    pub fields: Vec<TypeId>,
}

impl StructInfo {
    pub fn layout(&self, program: &Program) -> StructLayout {
        use std::alloc::Layout;
        let mut layout = Layout::from_size_align(0, 1).unwrap();
        let mut field_offsets = vec![];
        for field_ty in self
            .fields
            .iter()
            .map(|ty| program.types.get_info(*ty).unwrap())
        {
            let field_layout = field_ty.layout(program);
            let (new_layout, field_offset) = layout.extend(field_layout).unwrap();
            layout = new_layout;
            field_offsets.push(field_offset);
        }

        StructLayout {
            field_offsets,
            size: layout.size(),
            align: layout.align(),
        }
    }

    pub fn field_offset(&self, program: &Program, field_idx: usize) -> usize {
        self.layout(program).field_offsets[field_idx]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeInfo {
    PrimI32,
    PrimI64,
    PrimF32,
    PrimBool,
    Ref(TypeId),
    GcRef(TypeId),
    Struct(StructInfo),
}

impl TypeInfo {
    /// Returns `(size, align)`
    pub fn layout(&self, program: &Program) -> Layout {
        match self {
            TypeInfo::PrimBool => Layout::from_size_align(1, 1).unwrap(),
            TypeInfo::PrimI32 => Layout::from_size_align(4, 4).unwrap(),
            TypeInfo::PrimI64 => Layout::from_size_align(8, 8).unwrap(),
            TypeInfo::PrimF32 => Layout::from_size_align(4, 4).unwrap(),
            TypeInfo::Ref(_) => Layout::from_size_align(8, 8).unwrap(),
            TypeInfo::GcRef(_) => Layout::from_size_align(8, 8).unwrap(),
            TypeInfo::Struct(struct_info) => Layout::from_size_align(
                struct_info.layout(program).size,
                struct_info.layout(program).align,
            )
            .unwrap(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BasicBlock {
    instructions: Vec<Ins>,
    ins_final: InsFinal,
}

#[derive(Debug, Default, Clone)]
pub struct Func {
    name: String,
    entrypoint: BasicBlockId,
    blocks: SlotMap<BasicBlockId, BasicBlock>,
    locals: SlotMap<LocalId, TypeId>,
    args: Vec<LocalId>,
    return_type: Option<TypeId>,
}

#[derive(Default, Debug)]
struct TypesInternal {
    id2info: SlotMap<TypeId, TypeInfo>,
    info2id: HashMap<TypeInfo, TypeId>,
}

/// Interner for type definitions
#[derive(Default)]
pub struct Types {
    interner: Arc<Mutex<TypesInternal>>,
}

impl std::fmt::Debug for Types {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.interner.lock().unwrap().fmt(f)
    }
}

impl Types {
    pub fn i32(&self) -> TypeId {
        self.get_id(&TypeInfo::PrimI32)
    }

    pub fn i64(&self) -> TypeId {
        self.get_id(&TypeInfo::PrimI64)
    }

    pub fn f32(&self) -> TypeId {
        self.get_id(&TypeInfo::PrimF32)
    }

    pub fn bool(&self) -> TypeId {
        self.get_id(&TypeInfo::PrimBool)
    }

    pub fn ptr(&self) -> TypeId {
        self.get_id(&TypeInfo::Ref(self.i32()))
    }

    pub fn get_id(&self, info: &TypeInfo) -> TypeId {
        let mut inner = self.interner.lock().unwrap();
        match inner.info2id.get(info) {
            Some(id) => *id,
            None => {
                let id = inner.id2info.insert(info.clone());
                inner.info2id.insert(info.clone(), id);
                id
            }
        }
    }

    pub fn get_info(&self, id: TypeId) -> Option<TypeInfo> {
        let inner = self.interner.lock().unwrap();
        inner.id2info.get(id).cloned()
    }

    pub fn declare_type(&self) -> TypeId {
        let mut inner = self.interner.lock().unwrap();
        inner.id2info.insert(TypeInfo::PrimBool)
    }

    pub fn define_type(&self, ty: TypeId, info: TypeInfo) {
        let mut inner = self.interner.lock().unwrap();
        inner.id2info[ty] = info.clone();
        inner.info2id.insert(info, ty);
    }

    pub fn all_types(&self) -> Vec<(TypeId, TypeInfo)> {
        let inner = self.interner.lock().unwrap();
        inner
            .id2info
            .iter()
            .map(|(id, val)| (id, val.clone()))
            .collect_vec()
    }
}

#[derive(Debug)]
pub struct Program {
    entrypoint: FuncId,
    functions: SlotMap<FuncId, Func>,
    types: Types,
}
