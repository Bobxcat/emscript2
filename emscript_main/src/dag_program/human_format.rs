use itertools::{Itertools, PeekingNext};
use lalrpop_util::lalrpop_mod;
use slotmap::{Key, SlotMap};
use std::collections::HashMap;

use crate::dag_program::{
    BasicBlock, BasicBlockId, BinOp, Func, FuncId, InsFinal, LocalId, Operand, PlaceExpr, Program,
    Projection, StructInfo, TypeId, TypeInfo, Types, ValueExpr,
};

lalrpop_mod!(mir_grammar, "/dag_program/mir_grammar.rs");

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ParsedTypePath {
    TypeName(String),
    Ref(Box<ParsedTypePath>),
    GcRef(Box<ParsedTypePath>),
}

#[derive(Debug, Clone)]
enum ParsedProjection {
    Deref,
    Field(usize),
}

#[derive(Debug, Clone)]
struct ParsedPlace {
    root: String,
    projections: Vec<ParsedProjection>,
}

#[derive(Debug, Clone)]
enum ParsedOperand {
    Place(ParsedPlace),
    ConstI32(i32),
    ConstF32(f32),
    ConstBool(bool),
}

#[derive(Debug, Clone)]
enum ParsedValue {
    Alloc,
    Read(ParsedOperand),
    Ref(ParsedOperand),
    /// Performs a binary operation on a primitive type
    BinOp(String, ParsedOperand, ParsedOperand),
}

#[derive(Debug, Clone)]
struct ParsedBlockLine {
    lvalue: ParsedPlace,
    rvalue: ParsedValue,
}

#[derive(Debug, Clone)]
enum ParsedFunctionLine {
    DecLocal {
        name: String,
        ty: ParsedTypePath,
    },
    Block {
        name: String,
        lines: Vec<ParsedBlockLine>,
        term_ins: String,
        term_args: Vec<String>,
    },
}

#[derive(Debug, Clone)]
enum ParsedItem {
    Struct {
        name: String,
        fields: Vec<ParsedTypePath>,
    },
    Function {
        name: String,
        arg_locals: Vec<String>,
        lines: Vec<ParsedFunctionLine>,
        ret_ty: Option<ParsedTypePath>,
    },
}

pub fn parse_mir_program(program: &str, entrypoint: &str) -> Program {
    let parsed_items = match mir_grammar::DocumentParser::new().parse(program) {
        Ok(v) => v,
        Err(e) => panic!("ERROR: {e}"),
    };

    let mut functions: SlotMap<FuncId, Func> = SlotMap::default();
    let mut func_name_to_id: HashMap<String, FuncId> = HashMap::new();

    let types = Types::default();
    let mut type_name_to_id: HashMap<String, TypeId> = HashMap::new();
    type_name_to_id.insert("i32".into(), types.i32());
    type_name_to_id.insert("i64".into(), types.i64());
    type_name_to_id.insert("f32".into(), types.f32());
    type_name_to_id.insert("bool".into(), types.bool());

    // Declarations (types/functions)
    for parsed_item in &parsed_items {
        match parsed_item {
            ParsedItem::Struct { name, fields: _ } => {
                let ty = types.declare_type();
                type_name_to_id.insert(name.clone(), ty);
            }
            ParsedItem::Function {
                name,
                arg_locals: _,
                lines: _,
                ret_ty: _,
            } => {
                let f = functions.insert(Func::default());
                func_name_to_id.insert(name.clone(), f);
            }
        }
    }

    // Definitions
    for parsed_item in &parsed_items {
        let translate_type = |type_path: &ParsedTypePath| -> TypeId {
            fn inner(
                type_name_to_id: &HashMap<String, TypeId>,
                types: &Types,
                type_path: &ParsedTypePath,
            ) -> TypeId {
                match type_path {
                    ParsedTypePath::TypeName(x) => type_name_to_id[x],
                    ParsedTypePath::Ref(parsed_type_path) => {
                        let ty = types.declare_type();
                        let inner = inner(type_name_to_id, types, parsed_type_path);
                        types.define_type(ty, TypeInfo::Ref(inner));
                        ty
                    }
                    ParsedTypePath::GcRef(parsed_type_path) => {
                        let ty = types.declare_type();
                        let inner = inner(type_name_to_id, types, parsed_type_path);
                        types.define_type(ty, TypeInfo::GcRef(inner));
                        ty
                    }
                }
            }
            inner(&type_name_to_id, &types, type_path)
        };

        match parsed_item {
            ParsedItem::Struct { name, fields } => {
                let ty = type_name_to_id[name];
                types.define_type(
                    ty,
                    TypeInfo::Struct(StructInfo {
                        fields: fields
                            .iter()
                            .map(|f_path| translate_type(f_path))
                            .collect_vec(),
                    }),
                );
            }
            ParsedItem::Function {
                name,
                arg_locals,
                lines,
                ret_ty,
            } => {
                let f = func_name_to_id[name];
                let func = &mut functions[f];
                func.name = name.clone();
                func.return_type = ret_ty.as_ref().map(|ty| translate_type(ty));
                let mut local_name_to_id: HashMap<String, LocalId> = HashMap::new();
                let mut block_name_to_id: HashMap<String, BasicBlockId> = HashMap::new();

                // Declarations (locals/blocks)
                for line in lines {
                    match line {
                        ParsedFunctionLine::DecLocal { name, ty } => {
                            local_name_to_id
                                .insert(name.clone(), func.locals.insert(translate_type(ty)));
                        }
                        ParsedFunctionLine::Block {
                            name,
                            lines: _,
                            term_ins: _,
                            term_args: _,
                        } => {
                            block_name_to_id.insert(
                                name.clone(),
                                func.blocks.insert(BasicBlock {
                                    instructions: vec![],
                                    ins_final: InsFinal::Br {
                                        next: BasicBlockId::null(),
                                    },
                                }),
                            );
                        }
                    }
                }

                func.args = arg_locals
                    .iter()
                    .map(|name| local_name_to_id[name])
                    .collect_vec();

                // Definitions (locals/blocks)
                for line in lines {
                    match line {
                        ParsedFunctionLine::DecLocal { .. } => (),
                        ParsedFunctionLine::Block {
                            name,
                            lines,
                            term_ins,
                            term_args,
                        } => {
                            let translate_place_expr = |place: &ParsedPlace| -> PlaceExpr {
                                let local = local_name_to_id[&place.root];

                                PlaceExpr {
                                    local_ty: func.locals[local],
                                    local,
                                    projs: place
                                        .projections
                                        .iter()
                                        .map(|proj| match proj {
                                            ParsedProjection::Deref => Projection::Deref {},
                                            ParsedProjection::Field(idx) => {
                                                Projection::Field { field_idx: *idx }
                                            }
                                        })
                                        .collect(),
                                }
                            };

                            let translate_op = |op: &ParsedOperand| -> Operand {
                                match op {
                                    ParsedOperand::Place(place) => {
                                        Operand::Place(translate_place_expr(place))
                                    }
                                    ParsedOperand::ConstI32(x) => Operand::ConstI32(*x),
                                    ParsedOperand::ConstF32(x) => Operand::ConstF32(*x),
                                    ParsedOperand::ConstBool(x) => Operand::ConstBool(*x),
                                }
                            };

                            let bb_id = block_name_to_id[name];
                            let bb = &mut func.blocks[bb_id];

                            if func.entrypoint.is_null() {
                                func.entrypoint = bb_id;
                            }

                            for line in lines {
                                let lvalue = translate_place_expr(&line.lvalue);

                                if let ParsedValue::Alloc = &line.rvalue {
                                    bb.instructions
                                        .push(crate::dag_program::Ins::AllocGc { place: lvalue });
                                    continue;
                                }

                                let rvalue = match &line.rvalue {
                                    ParsedValue::Alloc => unreachable!(),
                                    ParsedValue::Read(op) => ValueExpr::Read(translate_op(op)),
                                    ParsedValue::Ref(op) => ValueExpr::Ref(translate_op(op)),
                                    ParsedValue::BinOp(op_name, op0, op1) => ValueExpr::BinOp(
                                        match &op_name.to_lowercase().trim()[..] {
                                            "add" => BinOp::Add,
                                            "sub" => BinOp::Sub,
                                            "mul" => BinOp::Mul,
                                            "div" => BinOp::Div,
                                            "eq" => BinOp::Eq,
                                            "le" => BinOp::Le,
                                            "lt" => BinOp::Lt,
                                            _ => panic!("Bad op_name: {op_name}"),
                                        },
                                        translate_op(op0),
                                        translate_op(op1),
                                    ),
                                };

                                bb.instructions.push(crate::dag_program::Ins::Assign {
                                    place: lvalue,
                                    value: rvalue,
                                });
                            }

                            bb.ins_final = match &term_ins.to_lowercase().trim()[..] {
                                "br" => InsFinal::Br {
                                    next: block_name_to_id[&term_args[0]],
                                },
                                "brif" => InsFinal::BrIf {
                                    cond: local_name_to_id[&term_args[0]],
                                    is_true: block_name_to_id[&term_args[1]],
                                    is_false: block_name_to_id[&term_args[2]],
                                },
                                "call" => {
                                    let mut args = term_args.into_iter();
                                    InsFinal::Call {
                                        func: func_name_to_id[args.next().unwrap()],
                                        store_ret: args
                                            .peeking_next(|x| local_name_to_id.contains_key(*x))
                                            .map(|x| local_name_to_id[x]),
                                        next: block_name_to_id[args.next().unwrap()],
                                        args: args.map(|name| local_name_to_id[name]).collect_vec(),
                                    }
                                }
                                "return" => InsFinal::Return {
                                    value: term_args.get(0).map(|arg| local_name_to_id[arg]),
                                },
                                _ => panic!("Bad terminal instruction: {term_ins}"),
                            };
                        }
                    }
                }
            }
        }
    }

    let program = Program {
        entrypoint: func_name_to_id[entrypoint],
        functions,
        types,
    };

    program
}
