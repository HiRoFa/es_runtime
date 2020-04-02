use crate::es_utils::EsErrorInfo;
use crate::esvaluefacade::EsValueFacade;
use crate::microtaskmanager::MicroTaskManager;
use crate::spidermonkeyruntimewrapper::SmRuntime;
use log::{debug, trace};
use std::sync::Arc;

pub type ImmutableJob<R> = Box<dyn FnOnce(&SmRuntime) -> R + Send + 'static>;
pub type MutableJob<R> = Box<dyn FnOnce(&mut SmRuntime) -> R + Send + 'static>;

pub struct EsRuntimeWrapperInner {
    pub(crate) task_manager: Arc<MicroTaskManager>,
    pub(crate) _pre_cleanup_tasks: Vec<Box<dyn Fn(&EsRuntimeWrapperInner) -> () + Send + Sync>>,
    pub(crate) module_source_loader: Option<Box<dyn Fn(&str) -> String + Send + Sync>>,
}

impl EsRuntimeWrapperInner {
    pub(crate) fn new() -> Self {
        EsRuntimeWrapperInner {
            task_manager: MicroTaskManager::new(),
            _pre_cleanup_tasks: vec![],
            module_source_loader: None,
        }
    }

    pub(crate) fn new_with_module_code_loader<C>(loader: C) -> Self
    where
        C: Fn(&str) -> String + Send + Sync + 'static,
    {
        EsRuntimeWrapperInner {
            task_manager: MicroTaskManager::new(),
            _pre_cleanup_tasks: vec![],
            module_source_loader: Some(Box::new(loader)),
        }
    }

    pub fn call(
        &self,
        obj_names: Vec<&'static str>,
        function_name: &str,
        args: Vec<EsValueFacade>,
    ) -> () {
        debug!("call {} in thread {}", function_name, thread_id::get());
        let f_n = function_name.to_string();

        self.do_in_es_runtime_thread(Box::new(move |sm_rt: &SmRuntime| {
            let res = sm_rt.call(obj_names, f_n.as_str(), args);
            if res.is_err() {
                debug!("async call failed: {}", res.err().unwrap().message);
            }
        }))
    }

    pub fn call_sync(
        &self,
        obj_names: Vec<&'static str>,
        function_name: &str,
        args: Vec<EsValueFacade>,
    ) -> Result<EsValueFacade, EsErrorInfo> {
        trace!("call_sync {} in thread {}", function_name, thread_id::get());
        let f_n = function_name.to_string();
        self.do_in_es_runtime_thread_sync(Box::new(move |sm_rt: &SmRuntime| {
            sm_rt.call(obj_names, f_n.as_str(), args)
        }))
    }

    pub fn eval(&self, eval_code: &str, file_name: &str) -> () {
        debug!("eval {} in thread {}", eval_code, thread_id::get());

        let eval_code = eval_code.to_string();
        let file_name = file_name.to_string();

        self.do_in_es_runtime_thread(Box::new(move |sm_rt: &SmRuntime| {
            let res = sm_rt.eval_void(eval_code.as_str(), file_name.as_str());
            if res.is_err() {
                debug!("async code eval failed: {}", res.err().unwrap().message);
            }
        }))
    }

    pub fn eval_sync(&self, code: &str, file_name: &str) -> Result<EsValueFacade, EsErrorInfo> {
        debug!("eval_sync {} in thread {}", code, thread_id::get());
        let eval_code = code.to_string();
        let file_name = file_name.to_string();

        self.do_in_es_runtime_thread_sync(Box::new(move |sm_rt: &SmRuntime| {
            sm_rt.eval(eval_code.as_str(), file_name.as_str())
        }))
    }

    pub fn eval_void_sync(&self, code: &str, file_name: &str) -> Result<(), EsErrorInfo> {
        let eval_code = code.to_string();
        let file_name = file_name.to_string();

        self.do_in_es_runtime_thread_sync(Box::new(move |sm_rt: &SmRuntime| {
            sm_rt.eval_void(eval_code.as_str(), file_name.as_str())
        }))
    }

    pub fn load_module_sync(
        &self,
        module_src: &str,
        module_file_name: &str,
    ) -> Result<(), EsErrorInfo> {
        let module_src_str = module_src.to_string();
        let module_file_name_str = module_file_name.to_string();

        self.do_in_es_runtime_thread_sync(Box::new(move |sm_rt: &SmRuntime| {
            sm_rt.load_module(module_src_str.as_str(), module_file_name_str.as_str())
        }))
    }

    pub(crate) fn cleanup_sync(&self) {
        trace!("cleaning up es_rt");
        // todo, set is_cleaning var on inner, here and now
        // that should hint the engine to not use this runtime
        self.do_in_es_runtime_thread_sync(Box::new(move |sm_rt: &SmRuntime| {
            sm_rt.cleanup();
        }));
        // reset cleaning var here
    }

    pub fn do_in_es_runtime_thread(&self, immutable_job: ImmutableJob<()>) -> () {
        trace!("do_in_es_runtime_thread");
        // this is executed in the single thread in the Threadpool, therefore Runtime and global are stored in a thread_local

        let job = || {
            let ret = crate::spidermonkeyruntimewrapper::SM_RT.with(|sm_rt| {
                debug!("got rt from thread_local");
                immutable_job(&mut sm_rt.borrow())
            });

            return ret;
        };

        self.task_manager.add_task(job);
    }
    pub fn do_in_es_runtime_thread_sync<R: Send + 'static>(
        &self,
        immutable_job: ImmutableJob<R>,
    ) -> R {
        trace!("do_in_es_runtime_thread_sync");
        // this is executed in the single thread in the Threadpool, therefore Runtime and global are stored in a thread_local

        let job = || {
            let ret = crate::spidermonkeyruntimewrapper::SM_RT.with(|sm_rt| {
                debug!("got rt from thread_local");
                immutable_job(&mut sm_rt.borrow())
            });

            ret
        };

        self.task_manager.exe_task(job)
    }

    pub fn do_in_es_runtime_thread_mut_sync(&self, mutable_job: MutableJob<()>) -> () {
        trace!("do_in_es_runtime_thread_mut_sync");
        // this is executed in the single thread in the Threadpool, therefore Runtime and global are stored in a thread_local

        let job = || {
            let ret = crate::spidermonkeyruntimewrapper::SM_RT.with(|sm_rt| {
                debug!("got rt from thread_local");
                mutable_job(&mut sm_rt.borrow_mut())
            });

            return ret;
        };

        self.task_manager.exe_task(job);
    }
    pub(crate) fn register_op(
        &self,
        name: &'static str,
        op: crate::spidermonkeyruntimewrapper::OP,
    ) {
        self.do_in_es_runtime_thread_mut_sync(Box::new(move |sm_rt: &mut SmRuntime| {
            sm_rt.register_op(name, op);
        }));
    }
}

impl Drop for EsRuntimeWrapperInner {
    fn drop(&mut self) {
        self.do_in_es_runtime_thread_mut_sync(Box::new(|_sm_rt: &mut SmRuntime| {
            debug!("dropping EsRuntimeWrapperInner");
        }));
    }
}
