use crate::esruntimewrapper::{EsRuntimeWrapper, ModuleCodeLoader};
use crate::esruntimewrapperinner::EsRuntimeWrapperInner;
use std::time::Duration;

/// The EsRuntimeWrapperBuilder struct can be used to initialize a new EsRuntimeWrapper
///
/// # Example
///
/// ```rust
/// use es_runtime::esruntimewrapperbuilder::EsRuntimeWrapperBuilder;
/// fn test_EsRuntimeWrapperBuilder(){
///     let rt = EsRuntimeWrapperBuilder::default().build();
/// }
/// ```
///

#[derive(Default)]
pub struct EsRuntimeWrapperBuilder {
    gc_interval: Option<Duration>,
    pub(crate) module_code_loader: Option<Box<ModuleCodeLoader>>,
    pub(crate) module_cache_size: usize,
    built: bool,
}

impl EsRuntimeWrapperBuilder {
    /// create a new instance of a EsRuntimeWrapperBuilder with it's default options
    pub fn new() -> Self {
        EsRuntimeWrapperBuilder {
            gc_interval: None,
            module_code_loader: None,
            module_cache_size: 50,
            built: false,
        }
    }

    /// set the gc_interval, if set this will start a new thread which will periodically call the garbage collector
    pub fn gc_interval(&mut self, interval: Duration) -> &mut Self {
        self.gc_interval = Some(interval);
        self
    }

    /// set a closure which is used to provide source code of modules
    pub fn module_code_loader(&mut self, loader: Box<ModuleCodeLoader>) -> &mut Self {
        self.module_code_loader = Some(loader);
        self
    }

    /// set the number of loaded modules you want to cache
    /// the modules are stored in a LruMap with a fixed max size
    pub fn module_cache_size(&mut self, size: usize) -> &mut Self {
        self.module_cache_size = size;
        self
    }

    /// build a new EsRuntimeWrapper based on the settings of this builder
    /// please note that this can be used only once
    pub fn build(&mut self) -> EsRuntimeWrapper {
        if self.built {
            panic!("cannot reuse builder");
        }

        self.built = true;

        // consume opts

        let mcl_opt: Option<Box<ModuleCodeLoader>> = if self.module_code_loader.is_some() {
            std::mem::replace(&mut self.module_code_loader, None)
        } else {
            None
        };

        let inner = EsRuntimeWrapperInner::build(mcl_opt, self.module_cache_size);
        let wrapper = EsRuntimeWrapper::new_inner(inner);
        if self.gc_interval.is_some() {
            wrapper.start_gc_deamon(self.gc_interval.unwrap());
        }
        wrapper
    }
}
