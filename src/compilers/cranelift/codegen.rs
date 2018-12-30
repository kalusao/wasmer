// cranelift
use cranelift_codegen::cursor::FuncCursor;
use cranelift_codegen::ir::immediates::{Offset32, Uimm64};
use cranelift_codegen::ir::types::{self, *};
use cranelift_codegen::ir::{
    self, AbiParam, ArgumentPurpose, ExtFuncData, ExternalName, FuncRef, InstBuilder, Signature,
    TrapCode,
};
use cranelift_codegen::isa::{self, CallConv, TargetFrontendConfig};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_entity::{EntityRef, PrimaryMap};
use cranelift_wasm::{
    translate_module, DefinedFuncIndex, FuncEnvironment as FuncEnvironmentTrait, FuncIndex,
    FuncTranslator, Global, GlobalIndex, GlobalVariable, Memory, MemoryIndex, ModuleEnvironment,
    ReturnMode, SignatureIndex, Table, TableIndex, WasmResult,
};
use target_lexicon;

// wasmer::webassembly
use crate::webassembly::errors::ErrorKind;

// wasmer::runtime
use crate::runtime::{
    module::{DataInitializer, Export, ImportName, Module as WasmerModule, TableInitializer},
    types::{
        Type as WasmerType,
        GlobalIndex as WasmerGlobalIndex,
        Global as WasmerGlobal,
        GlobalDesc as WasmerGlobalDesc,
        MemoryIndex as WasmerMemoryIndex,
        Memory as WasmerMemory,
        Table as WasmerTable,
        TableIndex as WasmerTableIndex,
        FuncIndex as WasmerFuncIndex,
        Initializer as WasmerInitializer,
        MapIndex,
    },
    vm::{
        self,
        Ctx as WasmerVMContext,
    },
};

use hashbrown::HashMap;

// std
use std::ptr::NonNull;

pub mod converter {
    use super::*;

    /// Converts a Cranelift module to wasmer module.
    pub fn convert_module(cranelift_module: super::CraneliftModule) -> WasmerModule {
        unimplemented!()
        // Generate globals, memories, tables, signatures_assoc
    }

    /// Converts from Cranelift type to wasmer type.
    pub fn convert_type(ty: types::Type) -> WasmerType {
        match ty {
            I32 => WasmerType::I32,
            I64 => WasmerType::I64,
            F32 => WasmerType::F32,
            F64 => WasmerType::F64,
            _ => unimplemented!("unsupported wasm type!"),
        }
    }

    /// Converts a Cranelift global to wasmer global.
    pub fn convert_global(global: Global) -> WasmerGlobal {
        // TODO: WasmerGlobal does not support `Import` as Global values
        let desc = WasmerGlobalDesc {
            mutable: global.mutability,
            ty: convert_type(global.ty),
        };

        use cranelift_wasm::GlobalInit::*;

        let init = match global.initializer {
            I32Const(val) => WasmerInitializer::Const(val.into()),
            I64Const(val) => WasmerInitializer::Const(val.into()),
            F32Const(val) => WasmerInitializer::Const((val as f32).into()),
            F64Const(val) => WasmerInitializer::Const((val as f64).into()),
            GetGlobal(index) =>
                WasmerInitializer::GetGlobal(
                    WasmerGlobalIndex::new(index.as_u32() as _)
                ),
            Import => unimplemented!("TODO: imported globals not supported yet!"),
        };

        WasmerGlobal {desc, init}
    }

    /// Converts a Cranelift global to wasmer global
    pub fn convert_table(table: Table) -> WasmerTable {
        unimplemented!()
    }
}

// Cranelift module for generating cranelift IR and the generic module
pub struct CraneliftModule {
    /// Target description relevant to frontends producing Cranelift IR.
    pub config: TargetFrontendConfig,

    /// Signatures as provided by `declare_signature`.
    pub signatures: Vec<ir::Signature>,

    /// Functions, imported and local.
    pub functions: PrimaryMap<FuncIndex, SignatureIndex>,

    /// Function bodies.
    pub function_bodies: PrimaryMap<DefinedFuncIndex, ir::Function>,

    /// The base of tables.
    pub tables_base: Option<ir::GlobalValue>,

    /// The Cranelift global holding the base address of the memories vector.
    pub memories_base: Option<ir::GlobalValue>,

    /// The Cranelift global holding the base address of the globals vector.
    pub globals_base: Option<ir::GlobalValue>,

    /// The external function declaration for implementing wasm's `current_memory`.
    pub current_memory_extfunc: Option<FuncRef>,

    /// The external function declaration for implementing wasm's `grow_memory`.
    pub grow_memory_extfunc: Option<FuncRef>,

    // ------------------------------------- //
    /// A function that takes a wasmer module and resolves a function index to a vm::Func.
    pub function_resolver:
        Option<Box<dyn Fn(&WasmerModule, WasmerFuncIndex) -> Option<NonNull<vm::Func>>>>,

    // An array holding information about the wasm instance memories.
    pub memories: Vec<Memory>,

    // An array holding information about the wasm instance globals.
    pub globals: Vec<Global>,

    // An array holding information about the wasm instance tables.
    pub tables: Vec<Table>,

    // An array holding information about the wasm instance imported functions.
    pub imported_functions: Vec<ImportName>,

    // An array holding information about the wasm instance imported memories.
    pub imported_memories: Vec<(ImportName, WasmerMemory)>,

    // An array holding information about the wasm instance imported tables.
    pub imported_tables: Vec<(ImportName, WasmerTable)>,

    // An array holding information about the wasm instance imported globals.
    pub imported_globals: Vec<(ImportName, WasmerGlobalDesc)>,

    // An hash map holding information about the wasm instance exports.
    pub exports: HashMap<String, Export>,

    // Data to initialize in memory.
    pub data_initializers: Vec<DataInitializer>,

    // Function indices to add to table.
    pub table_initializers: Vec<TableInitializer>,

    // The start function index.
    pub start_func: Option<WasmerFuncIndex>,
}

///
impl CraneliftModule {
    ///
    pub fn from_bytes(
        buffer_source: Vec<u8>,
        config: TargetFrontendConfig,
    ) -> Result<Self, ErrorKind> {
        // Create a cranelift module
        let mut cranelift_module = CraneliftModule {
            config,
            signatures: Vec::new(),
            functions: PrimaryMap::new(),
            function_bodies: PrimaryMap::new(),
            globals_base: None,
            tables_base: None,
            memories_base: None,
            current_memory_extfunc: None,
            grow_memory_extfunc: None,
            function_resolver: None,
            memories: Vec::new(),
            globals: Vec::new(),
            tables: Vec::new(),
            imported_functions: Vec::new(),
            imported_memories: Vec::new(),
            imported_tables: Vec::new(),
            imported_globals: Vec::new(),
            exports: HashMap::new(),
            data_initializers: Vec::new(),
            table_initializers: Vec::new(),
            start_func: None,
        };

        // Translate wasm to cranelift IR.
        translate_module(&buffer_source, &mut cranelift_module)
            .map_err(|e| ErrorKind::CompileError(e.to_string()))?;

        // Return translated module.
        Ok(cranelift_module)
    }

    ///
    fn func_env(&self) -> FuncEnvironment {
        FuncEnvironment::new(&self)
    }
}

///
pub struct FuncEnvironment<'environment> {
    pub module: &'environment CraneliftModule,
}

///
impl<'environment> FuncEnvironment<'environment> {
    pub fn new(module: &'environment CraneliftModule) -> Self {
        Self { module }
    }

    /// Creates a signature with VMContext as the last param
    pub fn generate_signature(&self, sigidx: SignatureIndex) -> ir::Signature {
        // Get signature
        let mut signature = self.module.signatures[sigidx.index()].clone();

        // Add the vmctx parameter type to it
        signature.params.push(ir::AbiParam::special(
            self.pointer_type(),
            ir::ArgumentPurpose::VMContext,
        ));

        // Return signature
        signature
    }
}

///
impl<'environment> FuncEnvironmentTrait for FuncEnvironment<'environment> {
    /// Gets configuration information needed for compiling functions
    fn target_config(&self) -> TargetFrontendConfig {
        self.module.config
    }

    /// Gets native pointers types.
    /// `I64` on 64-bit arch; `I32` on 32-bit arch.
    fn pointer_type(&self) -> ir::Type {
        ir::Type::int(u16::from(self.module.config.pointer_bits())).unwrap()
    }

    /// Gets the size of a native pointer in bytes.
    fn pointer_bytes(&self) -> u8 {
        self.module.config.pointer_bytes()
    }

    /// Set up the necessary preamble definitions in `func` to access the global identified
    /// by `index`.
    ///
    /// The index space covers both imported and locally declared globals.
    fn make_global(&mut self, func: &mut ir::Function, index: GlobalIndex) -> GlobalVariable {
        unimplemented!()
    }

    /// Set up the necessary preamble definitions in `func` to access the linear memory identified
    /// by `index`.
    ///
    /// The index space covers both imported and locally declared memories.
    fn make_heap(&mut self, func: &mut ir::Function, index: MemoryIndex) -> ir::Heap {
        unimplemented!()
    }

    /// Set up the necessary preamble definitions in `func` to access the table identified
    /// by `index`.
    ///
    /// The index space covers both imported and locally declared tables.
    fn make_table(&mut self, func: &mut ir::Function, index: TableIndex) -> ir::Table {
        unimplemented!()
    }

    /// Sets up a signature definition in `func`'s preamble
    /// Signature may contain additional argument, but arguments marked as ArgumentPurpose::Normal`
    /// must correspond to the arguments in the wasm signature
    fn make_indirect_sig(&mut self, func: &mut ir::Function, index: SignatureIndex) -> ir::SigRef {
        unimplemented!()
    }

    /// Set up an external function definition in the preamble of `func` that can be used to
    /// directly call the function `index`.
    ///
    /// The index space covers both imported functions and functions defined in the current module.
    fn make_direct_func(&mut self, func: &mut ir::Function, index: FuncIndex) -> ir::FuncRef {
        unimplemented!()
    }

    /// Generates an indirect call IR with `callee` and `call_args`
    /// Inserts instructions at `pos` to the function `callee` in the table
    /// `table_index` with WebAssembly signature `sig_index`
    #[cfg_attr(feature = "cargo-clippy", allow(too_many_arguments))]
    fn translate_call_indirect(
        &mut self,
        pos: FuncCursor,
        table_index: TableIndex,
        table: ir::Table,
        sig_index: SignatureIndex,
        sig_ref: ir::SigRef,
        callee: ir::Value,
        call_args: &[ir::Value],
    ) -> WasmResult<ir::Inst> {
        unimplemented!()
    }

    /// Generates a call IR with `callee` and `call_args` and inserts it at `pos`
    fn translate_call(
        &mut self,
        mut pos: FuncCursor,
        _callee_index: FuncIndex,
        callee: ir::FuncRef,
        call_args: &[ir::Value],
    ) -> WasmResult<ir::Inst> {
        Ok(pos.ins().call(callee, call_args))
    }

    /// Generates code corresponding to wasm `memory.grow`
    /// `index` refers to the linear memory to query.
    /// `heap` refers to the IR generated by `make_heap`.
    /// `val`  refers the value to grow the memory by.
    fn translate_memory_grow(
        &mut self,
        pos: FuncCursor,
        index: MemoryIndex,
        heap: ir::Heap,
        val: ir::Value,
    ) -> WasmResult<ir::Value> {
        unimplemented!()
    }

    /// Generates code corresponding to wasm `memory.size`
    /// `index` refers to the linear memory to query.
    /// `heap` refers to the IR generated by `make_heap`
    fn translate_memory_size(
        &mut self,
        pos: FuncCursor,
        index: MemoryIndex,
        heap: ir::Heap,
    ) -> WasmResult<ir::Value> {
        unimplemented!()
    }

    /// Generates code at the beginning of loops.
    /// Currently not used.
    fn translate_loop_header(&mut self, _pos: FuncCursor) {
        // By default, don't emit anything.
    }

    /// Determines the type of return each function should have.
    /// It normal returns for now.
    fn return_mode(&self) -> ReturnMode {
        ReturnMode::NormalReturns
    }
}

impl<'data> ModuleEnvironment<'data> for CraneliftModule {
    /// Get the information needed to produce Cranelift IR for the current target.
    fn target_config(&self) -> TargetFrontendConfig {
        self.config
    }

    /// Declares a function signature to the environment.
    fn declare_signature(&mut self, sig: &ir::Signature) {
        self.signatures.push(sig.clone());
        // TODO: push to signatures_assoc here.
    }

    /// Return the signature with the given index.
    fn get_signature(&self, sig_index: SignatureIndex) -> &ir::Signature {
        &self.signatures[sig_index.index()]
    }

    /// Declares a function import to the environment.
    fn declare_func_import(
        &mut self,
        sig_index: SignatureIndex,
        module: &'data str,
        field: &'data str,
    ) {
        // Imported functions are always declared first
        // Add signature index to list of functions
        self.functions.push(sig_index);

        // Add import names to list of imported functions
        self.imported_functions.push(
            (String::from(module), String::from(field)).into()
        );
    }

    /// Return the number of imported funcs.
    fn get_num_func_imports(&self) -> usize {
        self.imported_functions.len()
    }

    /// Declares the type (signature) of a local function in the module.
    fn declare_func_type(&mut self, sig_index: SignatureIndex) {
        self.functions.push(sig_index);
    }

    /// Return the signature index for the given function index.
    fn get_func_type(&self, func_index: FuncIndex) -> SignatureIndex {
        self.functions[func_index]
    }

    /// Declares a global to the environment.
    fn declare_global(&mut self, global: Global) {
        // Add global ir to the list of globals
        self.globals.push(global);
    }

    /// Declares a global import to the environment.
    fn declare_global_import(&mut self, global: Global, module: &'data str, field: &'data str) {
        // Add global index to list of globals
        self.globals.push(global);

        // Add import names to list of imported globals
        self.imported_globals.push((
            (String::from(module), String::from(field)).into(),
            converter::convert_global(global).desc,
        ));
    }

    /// Return the global for the given global index.
    fn get_global(&self, global_index: GlobalIndex) -> &Global {
        &self.globals[global_index.index()]
    }

    /// Declares a table to the environment.
    fn declare_table(&mut self, table: Table){
        // Add table ir to the list of tables
        self.tables.push(table);
    }

    /// Declares a table import to the environment.
    fn declare_table_import(&mut self, table: Table, module: &'data str, field: &'data str) {
        // // Add table index to list of tables
        // self.tables.push(table);

        // // Add import names to list of imported tables
        // self.imported_tables.push((
        //     (String::from(module), String::from(field)).into(),
        //     converter::convert_table(table).desc,
        // ));
        unimplemented!()
    }

    /// Fills a declared table with references to functions in the module.
    fn declare_table_elements(
        &mut self,
        table_index: TableIndex,
        base: Option<GlobalIndex>,
        offset: usize,
        elements: Vec<FuncIndex>,
    ) {
        unimplemented!()
    }

    /// Declares a memory to the environment
    fn declare_memory(&mut self, memory: Memory) {
        unimplemented!()
    }

    /// Declares a memory import to the environment.
    fn declare_memory_import(&mut self, memory: Memory, module: &'data str, field: &'data str) {
        unimplemented!()
    }

    /// Fills a declared memory with bytes at module instantiation.
    fn declare_data_initialization(
        &mut self,
        memory_index: MemoryIndex,
        base: Option<GlobalIndex>,
        offset: usize,
        data: &'data [u8],
    ) {
        unimplemented!()
    }

    /// Declares a function export to the environment.
    fn declare_func_export(&mut self, func_index: FuncIndex, name: &'data str) {
        unimplemented!()
    }
    /// Declares a table export to the environment.
    fn declare_table_export(&mut self, table_index: TableIndex, name: &'data str) {
        unimplemented!()
    }
    /// Declares a memory export to the environment.
    fn declare_memory_export(&mut self, memory_index: MemoryIndex, name: &'data str) {
        unimplemented!()
    }
    /// Declares a global export to the environment.
    fn declare_global_export(&mut self, global_index: GlobalIndex, name: &'data str) {
        unimplemented!()
    }

    /// Declares a start function.
    fn declare_start_func(&mut self, index: FuncIndex) {
        unimplemented!()
    }

    /// Provides the contents of a function body.
    fn define_function_body(&mut self, body_bytes: &'data [u8]) -> WasmResult<()> {
        unimplemented!()
    }
}