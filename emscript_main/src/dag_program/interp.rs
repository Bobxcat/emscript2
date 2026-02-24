use slotmap::{Key, SlotMap};

use crate::dag_program::{
    BasicBlockId, FuncId, HeapRef, Ins, InsFinal, LocalId, PlaceExpr, Program, Projection, TypeId,
    ValueExpr,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    /// Has yet to be given a value
    Undefined,
    Struct {
        ty: TypeId,
        fields: Vec<Value>,
    },
    StackRef {
        ty: TypeId,
        stack: LocalId,
    },
    HeapRef {
        ty: TypeId,
        heap: HeapRef,
    },
    PrimI32(i32),
    PrimF32(i32),
}

impl Program {
    fn type_of_value(&self, value: &Value) -> TypeId {
        match value {
            Value::Undefined => TypeId::null(),
            Value::Struct { ty, .. } => *ty,
            Value::StackRef { ty, .. } => *ty,
            Value::HeapRef { ty, .. } => *ty,
            Value::PrimI32(_) => self.type_primi32,
            Value::PrimF32(_) => self.type_primf32,
        }
    }
}

struct StackFrame {
    func: FuncId,
    curr_block: BasicBlockId,
    ins_idx: usize,
    locals: SlotMap<LocalId, Value>,
}

pub struct Interp {
    program: Program,
    heap: SlotMap<HeapRef, Value>,

    stack: Vec<StackFrame>,
}

impl Interp {
    pub fn new(program: Program) {
        //
    }
    pub fn step(&mut self) -> Result<(), ()> {
        let Some(stack_frame) = self.stack.last_mut() else {
            return Err(());
        };
        let func = &self.program.functions[stack_frame.func];
        let block = &func.blocks[stack_frame.curr_block];
        match &block.instructions.get(stack_frame.ins_idx) {
            Some(ins) => match ins {
                Ins::Assign { place, value } => {
                    let value_eval = {
                        match value {
                            ValueExpr::Read(place_expr) => {
                                let mut curr = &mut stack_frame.locals[place_expr.local];

                                for proj in &place_expr.projs {
                                    assert!(curr != &Value::Undefined);

                                    match proj {
                                        Projection::Field { field_idx } => match curr {
                                            Value::Struct { ty: _, fields } => {
                                                curr = &mut fields[*field_idx];
                                            }
                                            _ => panic!("Type mismatch"),
                                        },
                                        Projection::Index { idx_local } => todo!(),
                                        Projection::Deref {} => todo!(),
                                    }
                                }

                                curr
                                //
                            }
                            ValueExpr::Ref(place_expr) => todo!(),
                            ValueExpr::BinOp(bin_op, place_expr, place_expr1) => todo!(),
                        }
                    };
                    // value_eval.
                    //
                }
            },
            None => match &block.ins_final {
                InsFinal::Br { next, args } => todo!(),
                InsFinal::BrIf {
                    cond,
                    ge,
                    ge_args,
                    lt,
                    lt_args,
                } => todo!(),
                InsFinal::Return { value } => todo!(),
            },
        }
        //
        Ok(())
    }

    /*
    let a: i32;
    let b: i32;
    let c: i32;

    a = ;

    */

    #[track_caller]
    fn eval_place_expr<'a>(stack_frame: &'a mut StackFrame, expr: &PlaceExpr) -> &'a mut Value {
        todo!()
    }

    // #[track_caller]
    // fn eval_place_exprs(stack_frame: &mut StackFrame, exprs: Vec<&PlaceExpr>) -> Vec<&mut Value> {
    //     let mut evaluated = stack_frame.locals.locals[];
    // }

    #[track_caller]
    fn assign_op(program: &Program, lvalue: &mut Value, rvalue: Value) {
        let ltype = program.type_of_value(lvalue);
        let rtype = program.type_of_value(&rvalue);
        if ltype == rtype || ltype.is_null() {
            *lvalue = rvalue;
        } else {
            panic!("Mismatched types!");
        }
    }

    // pub fn step(&mut self) {
    // if self.curr_block.is_null() {
    //     return;
    // }
    // let block = &self.program.blocks[self.curr_block];
    // match block.instructions.get(self.ins_idx) {
    //     Some(ins) => {
    //         match ins {
    //             Ins::Assign { place, value } => {
    //                 //
    //             }
    //         }
    //     }
    //     None => {
    //         let next_block: BasicBlockId;
    //         let next_block_args: Vec<LocalValue>;
    //         match &block.ins_final {
    //             InsFinal::Br { next, args } => {
    //                 next_block = next.clone();
    //                 next_block_args = args.clone();
    //             }
    //             InsFinal::BrIf {
    //                 cond,
    //                 ge,
    //                 ge_args,
    //                 lt,
    //                 lt_args,
    //             } => todo!(),
    //         }

    //         self.ins_idx = 0;
    //         self.curr_block = next_block;

    //         //
    //     }
    // }

    // //
    // }
}

// Program
// Function
// ^ Stack frame, defining (not initializing) all local variables in the function
// Basic Block

// When *returning* or *using as a parameter* a ref to a PlaceExpr derived from a local, promote that local into the heap
// This simple version can easily be precomputed at compile time
