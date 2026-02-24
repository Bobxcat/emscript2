use std::marker::PhantomData;

use slotmap::SlotMap;

slotmap::new_key_type! {
    pub struct ModId;
    pub struct VarId;
    pub struct StructId;
}

pub trait IdType {
    //
}

pub struct ModInfo {
    pub name: String,
    pub parent: Option<ModId>,
    pub submods: Vec<ModId>,
}

pub struct VarInfo {
    pub name: String,
}

pub struct TypeInfo {
    //
}

pub struct AbsStore {
    pub modules: SlotMap<ModId, ModInfo>,
    pub vars: SlotMap<VarId, VarInfo>,
    pub structs: SlotMap<StructId, TypeInfo>,
}

/// Absolute path
pub struct Abs {
    pub module: ModId,
    pub var: VarId,
    pub kind: StructId,
}

/// Relative path
pub struct Rel {
    pub module: ModId,
    pub name: String,
}

pub enum ASTNodeKind<ID: IdType> {
    LoadVar {
        //
        value: ID,
    },
}

pub struct ASTNode<ID: IdType> {
    kind: ASTNodeKind<ID>,
}

pub struct AST<ID: IdType> {
    // id_store: AbsStore,
    _p: PhantomData<ID>,
}

fn resolve_local_var() {
    //
}

mod foo {
    use slotmap::SlotMap;

    slotmap::new_key_type! {
        pub struct RawASTNodeId;
        pub struct ItemId;
        pub struct VarId;
    }

    pub enum ItemKind {
        Mod { name: String },
        Scope,
    }

    pub struct Item {
        pub parent: ItemId,
    }

    pub struct Var {
        pub scope: ItemId,
    }

    pub struct CanonicalStore {
        items: SlotMap<ItemId, Item>,
        vars: SlotMap<VarId, Var>,
    }

    pub enum RawASTNodeKind {
        Mod {
            name: String,
            body: RawASTNodeId,
        },
        Func {
            name: String,
            body: RawASTNodeId,
        },
        VarDefine {
            var: String,
        },
        Assign {
            lhs: RawASTNodeId,
            rhs: RawASTNodeId,
        },
    }

    // pub struct RawASTNode {
    //     kind: ,
    // }

    // pub struct RawAST {
    //     node:
    // }

    fn canonicalize() {
        // Append current item to the current item path
        // Register current item in the store
    }
}
