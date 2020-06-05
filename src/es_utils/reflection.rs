//!
//! # Reflection
//!
//! Using rust data from script
//!
//! U can use the Proxy struct in es_utils::reflection to create a proxy object
//!
//! ```rust
//! use mozjs::rooted;
//! use mozjs::rust::HandleValue;
//! use mozjs::jsval::{Int32Value, UndefinedValue};
//! use mozjs::jsapi::JSContext;
//! use es_runtime::es_utils;
//! use log::debug;
//! use es_runtime::esruntimewrapperbuilder::EsRuntimeWrapperBuilder;
//! fn test_proxy() {
//!     let rt = EsRuntimeWrapperBuilder::new().build();
//!     rt.do_in_es_runtime_thread_sync(|sm_rt| {
//!         sm_rt.do_with_jsapi(|_rt, cx, global|{
//!             let proxy_arc = es_utils::reflection::ProxyBuilder::new(vec![], "TestClass1")
//!             // if you want to create a proxy that can be constructed you need to pass a constructor
//!             // you need to generate an id here for your object
//!             // if you don't pass a constructor you can only use the static_* methods to create events, properties and methods
//!            .constructor(|_cx: *mut JSContext, args: &Vec<HandleValue>| {
//!                // this will run in the sm_rt workerthread so global is rooted here
//!                debug!("proxytest: construct");
//!                Ok(1)
//!            })
//!            // create a property and pass a closure for the get and set action
//!            .property("foo", |_cx, obj_id| {
//!                debug!("proxy.get foo {}", obj_id);
//!                Ok(Int32Value(123))
//!            }, |_cx, obj_id, _val| {
//!                debug!("proxy.set foo {}", obj_id);
//!                Ok(())
//!            })
//!            // the finalizer is called when the instance is garbage collected, use this to drop your own object in rust
//!            .finalizer(|id: &i32| {
//!                debug!("proxytest: finalize id {}", id);
//!            })
//!            // a method for your instance
//!            .method("methodA", |_cx, obj_id, args| {
//!                debug!("proxy.methodA called for obj {} with {} args", obj_id, args.len());
//!                Ok(UndefinedValue())
//!            })
//!            // and an event that may be dispatched
//!            .event("saved")
//!            // when done build your proxy
//!            .build(cx, global);
//!
//!
//!                let esvf = sm_rt.eval(
//!                "// create a new instance of your Proxy\n\
//!                     let tp_obj = new TestClass1('bar'); \n\
//!                 // you can set props that are not proxied \n\
//!                     tp_obj.abc = 1; console.log('tp_obj.abc = %s', tp_obj.abc); \n\
//!                // test you getter and setter\n\
//!                    let i = tp_obj.foo; tp_obj.foo = 987; \n\
//!                // test your method\n\
//!                    tp_obj.methodA(1, 2, 3); \n\
//!                // add an event listener\n\
//!                     tp_obj.addEventListener('saved', (evt) => {console.log('tp_obj was saved');}); \n\
//!                // dispatch an event from script\n\
//!                    tp_obj.dispatchEvent('saved', {}); \n\
//!                // allow you object to be GCed\n\
//!                    tp_obj = null; i;",
//!                "test_proxy.es",
//!            ).ok().unwrap();
//!
//!            assert_eq!(&123, esvf.get_i32());
//!
//!            // dispatch event from rust
//!            rooted!(in (cx) let event_obj_root = UndefinedValue());
//!            proxy_arc.dispatch_event(1, "saved", cx, event_obj_root.handle().into());
//!     });
//! })
//!}
//! ```
//!

use crate::es_utils::rooting::EsPersistentRooted;
use crate::es_utils::{es_jsid_to_string, EsErrorInfo};

use mozjs::jsapi::CallArgs;
use mozjs::jsapi::HandleValueArray;
use mozjs::jsapi::JSClass;
use mozjs::jsapi::JSClassOps;
use mozjs::jsapi::JSContext;
use mozjs::jsapi::JSFreeOp;
use mozjs::jsapi::JSNative;
use mozjs::jsapi::JSObject;
use mozjs::jsapi::JS_ReportErrorASCII;
use mozjs::jsapi::JSCLASS_FOREGROUND_FINALIZE;
use mozjs::jsval::{JSVal, NullValue, ObjectValue, UndefinedValue};
use mozjs::rust::{HandleObject, HandleValue, MutableHandleObject};

use core::ptr;
use log::trace;
use std::borrow::Borrow;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ptr::replace;
use std::sync::Arc;

pub type Constructor = Box<dyn Fn(*mut JSContext, &Vec<HandleValue>) -> Result<i32, String>>;
pub type Setter = Box<dyn Fn(*mut JSContext, i32, HandleValue) -> Result<(), String>>;
pub type Getter = Box<dyn Fn(*mut JSContext, i32) -> Result<JSVal, String>>;
pub type StaticSetter = Box<dyn Fn(*mut JSContext, HandleValue) -> Result<(), String>>;
pub type StaticGetter = Box<dyn Fn(*mut JSContext) -> Result<JSVal, String>>;
pub type Method = Box<dyn Fn(*mut JSContext, i32, &Vec<HandleValue>) -> Result<JSVal, String>>;
pub type StaticMethod = Box<dyn Fn(*mut JSContext, &Vec<HandleValue>) -> Result<JSVal, String>>;

/// create a class def in the runtime which constructs and calls methods in a rust proxy
pub struct Proxy {
    pub namespace: Vec<&'static str>,
    pub class_name: &'static str,
    constructor: Option<Constructor>,
    finalizer: Option<Box<dyn Fn(&i32) -> ()>>,
    properties: HashMap<&'static str, (Getter, Setter)>,

    // todo add cx as second arg to methods
    methods: HashMap<&'static str, Method>,
    native_methods: HashMap<&'static str, JSNative>,
    events: HashSet<&'static str>,
    event_listeners: RefCell<HashMap<i32, HashMap<&'static str, Vec<EsPersistentRooted>>>>,
    static_properties: HashMap<&'static str, (StaticGetter, StaticSetter)>,
    static_methods: HashMap<&'static str, StaticMethod>,
    static_native_methods: HashMap<&'static str, JSNative>,
    static_events: HashSet<&'static str>,
    static_event_listeners: RefCell<HashMap<&'static str, Vec<EsPersistentRooted>>>,
}

pub struct ProxyBuilder {
    pub namespace: Vec<&'static str>,
    pub class_name: &'static str,
    constructor: Option<Constructor>,
    finalizer: Option<Box<dyn Fn(&i32) -> ()>>,
    properties: HashMap<&'static str, (Getter, Setter)>,
    methods: HashMap<&'static str, Method>,
    native_methods: HashMap<&'static str, JSNative>,
    events: HashSet<&'static str>,
    static_properties: HashMap<&'static str, (StaticGetter, StaticSetter)>,
    static_methods: HashMap<&'static str, StaticMethod>,
    static_native_methods: HashMap<&'static str, JSNative>,
    static_events: HashSet<&'static str>,
}

thread_local! {
    static PROXY_INSTANCE_IDS: RefCell<HashMap<usize, i32>> = RefCell::new(HashMap::new());
    static PROXY_INSTANCE_CLASSNAMES: RefCell<HashMap<i32, String>> = RefCell::new(HashMap::new());
    static PROXIES: RefCell<HashMap<String, Arc<Proxy>>> = RefCell::new(HashMap::new());
}

/// find a ref to a proxy, use full canonical name as key
pub fn get_proxy(canonical_name: &str) -> Option<Arc<Proxy>> {
    // get proxy from PROXIES
    PROXIES.with(|rc: &RefCell<HashMap<String, Arc<Proxy>>>| {
        let map: &HashMap<String, Arc<Proxy>> = &*rc.borrow();
        map.get(canonical_name).cloned()
    })
}

impl Proxy {
    fn new(cx: *mut JSContext, scope: HandleObject, builder: &mut ProxyBuilder) -> Arc<Self> {
        let mut ret = Proxy {
            namespace: builder.namespace.clone(),
            class_name: builder.class_name,
            constructor: unsafe { replace(&mut builder.constructor, None) },
            finalizer: unsafe { replace(&mut builder.finalizer, None) },
            properties: HashMap::new(),
            methods: HashMap::new(),
            native_methods: HashMap::new(),
            events: HashSet::new(),
            event_listeners: RefCell::new(HashMap::new()),
            static_properties: HashMap::new(),
            static_methods: HashMap::new(),
            static_native_methods: HashMap::new(),
            static_events: HashSet::new(),
            static_event_listeners: RefCell::new(HashMap::new()),
        };

        builder.properties.drain().all(|e| {
            ret.properties.insert(e.0, e.1);
            true
        });

        builder.methods.drain().all(|e| {
            ret.methods.insert(e.0, e.1);
            true
        });

        builder.native_methods.drain().all(|e| {
            ret.native_methods.insert(e.0, e.1);
            true
        });

        builder.events.drain().all(|evt_type| {
            ret.events.insert(evt_type);
            true
        });

        builder.static_properties.drain().all(|e| {
            ret.static_properties.insert(e.0, e.1);
            true
        });

        builder.static_methods.drain().all(|e| {
            ret.static_methods.insert(e.0, e.1);
            true
        });

        builder.static_native_methods.drain().all(|e| {
            ret.static_native_methods.insert(e.0, e.1);
            true
        });

        builder.static_events.drain().all(|evt_type| {
            ret.static_events.insert(evt_type);
            true
        });

        // todo if no constructor: create an object instead of a function

        // todo get_or_define with rval
        let pkg_obj =
            crate::es_utils::objects::get_or_define_namespace(cx, scope, ret.namespace.clone());
        rooted!(in (cx) let pkg_root = pkg_obj);

        let func: *mut mozjs::jsapi::JSFunction =
            crate::es_utils::functions::define_native_constructor(
                cx,
                pkg_root.handle(),
                ret.class_name,
                Some(proxy_construct),
            );

        let cname = ret.get_canonical_name();
        rooted!(in (cx) let cname_root = crate::es_utils::new_es_value_from_str(cx, cname.as_str()));
        crate::es_utils::objects::set_es_obj_prop_val_permanent(
            cx,
            unsafe { HandleObject::from_marked_location(&(func as *mut JSObject)) },
            PROXY_PROP_CLASS_NAME,
            cname_root.handle(),
        );

        ret.init_static_properties(cx, unsafe {
            mozjs::rust::HandleObject::from_marked_location(&(func as *mut JSObject))
        });
        ret.init_static_methods(cx, unsafe {
            mozjs::rust::HandleObject::from_marked_location(&(func as *mut JSObject))
        });
        ret.init_static_events(cx, unsafe {
            mozjs::rust::HandleObject::from_marked_location(&(func as *mut JSObject))
        });

        let ret_arc = Arc::new(ret);

        PROXIES.with(|map_rc: &RefCell<HashMap<String, Arc<Proxy>>>| {
            let map = &mut *map_rc.borrow_mut();
            map.insert(ret_arc.get_canonical_name(), ret_arc.clone());
        });

        ret_arc
    }

    pub fn get_canonical_name(&self) -> String {
        format!("{}.{}", self.namespace.join("."), self.class_name)
    }

    pub fn new_instance(
        &self,
        cx: *mut JSContext,
        scope: HandleObject,
        args: HandleValueArray,
        return_handle: MutableHandleObject,
    ) -> Result<(), EsErrorInfo> {
        // to be used for EsValueFacade::new_proxy()

        let namespace = vec!["a", "b", "Client"];
        let constructor_obj =
            crate::es_utils::objects::get_or_define_namespace(cx, scope, namespace);
        rooted!(in (cx) let constructor_root = ObjectValue(constructor_obj));
        crate::es_utils::objects::new_from_constructor(
            cx,
            constructor_root.handle(),
            args,
            return_handle,
        )
    }

    pub fn dispatch_event(
        &self,
        obj_id: i32,
        event_name: &str,
        cx: *mut JSContext,
        event_obj: mozjs::jsapi::HandleValue,
    ) {
        dispatch_event_for_proxy(cx, self, obj_id, event_name, event_obj);
    }

    pub fn dispatch_static_event(
        &self,
        event_name: &str,
        cx: *mut JSContext,
        event_obj: mozjs::jsapi::HandleValue,
    ) {
        dispatch_static_event_for_proxy(cx, self, event_name, event_obj);
    }

    fn init_static_properties(&self, cx: *mut JSContext, func: HandleObject) {
        // this is actually how static_props should work, not instance props.. they should be resolved from the proxy_op
        for prop_name in self.static_properties.keys() {
            // https://doc.servo.org/mozjs/jsapi/fn.JS_DefineProperty1.html
            // mozjs::jsapi::JS_DefineProperty1
            // todo move this to es_utils::object
            let n = format!("{}\0", prop_name);

            let ok = unsafe {
                mozjs::jsapi::JS_DefineProperty1(
                    cx,
                    func.into(),
                    n.as_ptr() as *const libc::c_char,
                    Some(proxy_static_getter),
                    Some(proxy_static_setter),
                    (mozjs::jsapi::JSPROP_PERMANENT
                        & mozjs::jsapi::JSPROP_GETTER
                        & mozjs::jsapi::JSPROP_SETTER) as u32,
                )
            };
            assert!(ok);
        }
    }

    fn init_static_methods(&self, cx: *mut JSContext, func: HandleObject) {
        trace!("init static methods for {}", self.class_name);
        for method_name in self.static_methods.keys() {
            trace!("init static method {} for {}", method_name, self.class_name);
            crate::es_utils::functions::define_native_function(
                cx,
                func,
                method_name,
                Some(proxy_static_method),
            );
        }
        for native_method_name in self.static_native_methods.keys() {
            trace!(
                "init static method {} for {}",
                native_method_name,
                self.class_name
            );
            let method: JSNative = self
                .static_native_methods
                .get(native_method_name)
                .cloned()
                .unwrap();
            crate::es_utils::functions::define_native_function(
                cx,
                func,
                native_method_name,
                method,
            );
        }
    }
    fn init_static_events(&self, cx: *mut JSContext, func: HandleObject) {
        crate::es_utils::functions::define_native_function(
            cx,
            func,
            "addEventListener",
            Some(proxy_static_add_event_listener),
        );
        crate::es_utils::functions::define_native_function(
            cx,
            func,
            "removeEventListener",
            Some(proxy_static_remove_event_listener),
        );
        crate::es_utils::functions::define_native_function(
            cx,
            func,
            "dispatchEvent",
            Some(proxy_static_dispatch_event),
        );
    }
}

impl ProxyBuilder {
    pub fn new(namespace: Vec<&'static str>, class_name: &'static str) -> Self {
        ProxyBuilder {
            namespace,
            class_name,
            constructor: None,
            finalizer: None,
            properties: HashMap::new(),
            methods: HashMap::new(),
            native_methods: HashMap::new(),
            events: HashSet::new(),
            static_properties: HashMap::new(),
            static_methods: HashMap::new(),
            static_native_methods: HashMap::new(),
            static_events: HashSet::new(),
        }
    }

    pub fn constructor<C>(&mut self, constructor: C) -> &mut Self
    where
        C: Fn(*mut JSContext, &Vec<HandleValue>) -> Result<i32, String> + 'static,
    {
        self.constructor = Some(Box::new(constructor));
        self
    }

    pub fn finalizer<F>(&mut self, finalizer: F) -> &mut Self
    where
        F: Fn(&i32) -> () + 'static,
    {
        self.finalizer = Some(Box::new(finalizer));
        self
    }

    pub fn property<G, S>(&mut self, name: &'static str, getter: G, setter: S) -> &mut Self
    where
        G: Fn(*mut JSContext, i32) -> Result<JSVal, String> + 'static,
        S: Fn(*mut JSContext, i32, HandleValue) -> Result<(), String> + 'static,
    {
        self.properties
            .insert(name, (Box::new(getter), Box::new(setter)));
        self
    }

    pub fn static_property<G, S>(&mut self, name: &'static str, getter: G, setter: S) -> &mut Self
    where
        G: Fn(*mut JSContext) -> Result<JSVal, String> + 'static,
        S: Fn(*mut JSContext, HandleValue) -> Result<(), String> + 'static,
    {
        self.static_properties
            .insert(name, (Box::new(getter), Box::new(setter)));
        self
    }

    pub fn method<M>(&mut self, name: &'static str, method: M) -> &mut Self
    where
        M: Fn(*mut JSContext, i32, &Vec<HandleValue>) -> Result<JSVal, String> + 'static,
    {
        self.methods.insert(name, Box::new(method));
        self
    }

    pub fn native_method<M>(&mut self, name: &'static str, method: JSNative) -> &mut Self {
        self.native_methods.insert(name, method);
        self
    }

    pub fn static_method<M>(&mut self, name: &'static str, method: M) -> &mut Self
    where
        M: Fn(*mut JSContext, &Vec<HandleValue>) -> Result<JSVal, String> + 'static,
    {
        self.static_methods.insert(name, Box::new(method));
        self
    }

    pub fn static_native_method(&mut self, name: &'static str, method: JSNative) -> &mut Self {
        self.static_native_methods.insert(name, method);
        self
    }

    pub fn build(&mut self, cx: *mut JSContext, scope: HandleObject) -> Arc<Proxy> {
        Proxy::new(cx, scope, self)
    }

    pub fn event(&mut self, evt_type: &'static str) -> &mut Self {
        self.events.insert(evt_type);
        self
    }

    pub fn static_event(&mut self, evt_type: &'static str) -> &mut Self {
        self.static_events.insert(evt_type);
        self
    }
}

#[cfg(test)]
mod tests {
    use crate::es_utils::es_value_to_str;
    use crate::es_utils::reflection::*;
    use crate::spidermonkeyruntimewrapper::SmRuntime;
    use log::debug;
    use mozjs::jsval::{Int32Value, UndefinedValue};
    use mozjs::rust::HandleValue;

    #[test]
    fn test_proxy() {
        log::info!("test_proxy");
        let rt = crate::esruntimewrapper::tests::TEST_RT.clone();

        rt.do_with_inner(|inner| {
            inner.do_in_es_runtime_thread_sync(|sm_rt: &SmRuntime| {
                sm_rt.do_with_jsapi(|_rt, cx, global| {
                    let _proxy_arc = ProxyBuilder::new(vec![],"TestClass1")
                        .constructor(|cx: *mut JSContext, args: &Vec<HandleValue>| {
                            // this will run in the sm_rt workerthread so global is rooted here
                            debug!("proxytest: construct");
                            let name = if !args.is_empty() {
                                let hv = args.get(0).unwrap();
                                es_value_to_str(cx, **hv).ok().unwrap()
                            } else {
                                "NoName".to_string()
                            };
                            debug!("proxytest: construct with name {}", name);
                            Ok(1)
                        })
                        .property("foo", |_cx, obj_id| {
                            debug!("proxy.get foo {}", obj_id);
                            Ok(Int32Value(123))
                        }, |_cx, obj_id, _val| {
                            debug!("proxy.set foo {}", obj_id);
                            Ok(())
                        })
                        .property("bar", |_cx, _obj_id| {
                            Ok(Int32Value(456))
                        }, |_cx, _obj_id, _val| {
                            Ok(())
                        })
                        .finalizer(|id: &i32| {
                            debug!("proxytest: finalize id {}", id);
                        })
                        .method("methodA", |_cx, obj_id, args| {
                            trace!("proxy.methodA called for obj {} with {} args", obj_id, args.len());
                            Ok(UndefinedValue())
                        })
                        .method("methodB", |_cx, obj_id, args| {
                            trace!("proxy.methodB called for obj {} with {} args", obj_id, args.len());
                            Ok(UndefinedValue())
                        })
                        .event("saved")
                        .build(cx, global);
                    let esvf = sm_rt
                        .eval(
                            "// create a new instance of your Proxy\n\
                                      let tp_obj = new TestClass1('bar'); \n\
                                      // you can set props that are not proxied \n\
                                      tp_obj.abc = 1; console.log('tp_obj.abc = %s', tp_obj.abc); \n\
                                      // test you getter and setter\n\
                                      let tp_i = tp_obj.foo; tp_obj.foo = 987; \n\
                                      // test your method\n\
                                      tp_obj.methodA(1, 2, 3);tp_obj.methodA(1, 2, 3);tp_obj.methodA(1, 2, 3); \n\
                                      tp_obj.methodB(true); \n\
                                      // add an event listener\n\
                                      tp_obj.addEventListener('saved', (evt) => {console.log('tp_obj was saved');}); \n\
                                      // dispatch an event from script\n\
                                      tp_obj.dispatchEvent('saved', {}); \n\
                                      // allow you object to be GCed\n\
                                      tp_obj = null; tp_i;",
                            "test_proxy.es",
                        )
                        .ok()
                        .unwrap();
                    assert_eq!(&123, esvf.get_i32());
                });
            });
            inner.do_in_es_runtime_thread_sync(|sm_rt: &SmRuntime| {
                sm_rt.cleanup();
            });
        });
    }

    #[test]
    fn test_static_proxy() {
        log::info!("test_static_proxy");
        let rt = crate::esruntimewrapper::tests::TEST_RT.clone();

        rt.do_with_inner(|inner| {
            inner.do_in_es_runtime_thread_sync(|sm_rt: &SmRuntime| {
                sm_rt.do_with_jsapi(|_rt, cx, global| {
                    let _proxy_arc = ProxyBuilder::new(vec![],"TestClass2")
                        .static_property("foo", |_cx| {
                            debug!("static_proxy.get foo");
                            Ok(Int32Value(123))
                        }, |_cx, _val| {
                            debug!("static_proxy.set foo");
                            Ok(())
                        })
                        .static_property("bar", |_cx| {
                            Ok(Int32Value(456))
                        }, |_cx, _val| {
                            Ok(())
                        })
                        .static_method("methodA", |_cx, args| {
                            trace!("static_proxy.methodA called with {} args", args.len());
                            Ok(UndefinedValue())
                        })
                        .static_method("methodB", |_cx, args| {
                            trace!("static_proxy.methodB called with {} args", args.len());
                            Ok(UndefinedValue())
                        })
                        .static_event("saved")
                        .build(cx, global);
                    let esvf = sm_rt
                        .eval(
                            "// you can set props that are not proxied \n\
                                      TestClass2.abc = 1; console.log('TestClass2.abc = %s', TestClass2.abc); \n\
                                      // test you getter and setter\n\
                                      let tsp_i = TestClass2.foo; TestClass2.foo = 987; \n\
                                      // test your method\n\
                                      TestClass2.methodA(1, 2, 3);TestClass2.methodA(1, 2, 3);TestClass2.methodA(1, 2, 3); \n\
                                      TestClass2.methodB(true); \n\
                                      // add an event listener\n\
                                      TestClass2.addEventListener('saved', (evt) => {console.log('TestClass2 was saved');}); \n\
                                      // dispatch an event from script\n\
                                      TestClass2.dispatchEvent('saved', {}); tsp_i;",
                            "test_static_proxy.es",
                        )
                        .ok()
                        .unwrap();
                    assert_eq!(&123, esvf.get_i32());
                });
            });
            inner.do_in_es_runtime_thread_sync(|sm_rt: &SmRuntime| {
                sm_rt.cleanup();
            });
        });
    }
}

static ES_PROXY_CLASS_CLASS_OPS: JSClassOps = JSClassOps {
    addProperty: None,
    delProperty: None,
    enumerate: None,
    newEnumerate: None,
    resolve: Some(proxy_instance_resolve),
    mayResolve: None,
    finalize: Some(proxy_instance_finalize),
    call: None,
    hasInstance: None,
    construct: None,
    trace: None,
};

static ES_PROXY_CLASS: JSClass = JSClass {
    name: b"EsProxy\0" as *const u8 as *const libc::c_char,
    flags: JSCLASS_FOREGROUND_FINALIZE,
    cOps: &ES_PROXY_CLASS_CLASS_OPS as *const JSClassOps,
    spec: ptr::null(),
    ext: ptr::null(),
    oOps: ptr::null(),
};

/// resolvea property, this means if we know how to handle a prop we define that prop ob the instance obj
unsafe extern "C" fn proxy_instance_resolve(
    cx: *mut JSContext,
    obj: mozjs::jsapi::HandleObject,
    key: mozjs::jsapi::HandleId,
    resolved: *mut bool,
) -> bool {
    trace!("reflection::resolve");

    let prop_name = es_jsid_to_string(cx, key);

    trace!("reflection::resolve {}", prop_name);

    let rhandle = mozjs::rust::HandleObject::from_marked_location(&obj.get());
    let class_name_res =
        crate::es_utils::objects::get_es_obj_prop_val_as_string(cx, rhandle, PROXY_PROP_CLASS_NAME);
    if let Ok(class_name) = class_name_res {
        PROXIES.with(|proxies_rc| {
            let proxies = &*proxies_rc.borrow();
            if let Some(proxy) = proxies.get(class_name.as_str()) {
                trace!("check proxy {} for {}", class_name, prop_name);

                if prop_name.as_str().eq("addEventListener") {
                    trace!("define addEventListener");

                    let robj = mozjs::rust::HandleObject::from_marked_location(&obj.get());
                    crate::es_utils::functions::define_native_function(
                        cx,
                        robj,
                        "addEventListener",
                        Some(proxy_instance_add_event_listener),
                    );

                    *resolved = true;
                    trace!("resolved addEventListener {}", prop_name);
                } else if prop_name.as_str().eq("removeEventListener") {
                    trace!("define removeEventListener");

                    let robj = mozjs::rust::HandleObject::from_marked_location(&obj.get());
                    crate::es_utils::functions::define_native_function(
                        cx,
                        robj,
                        "removeEventListener",
                        Some(proxy_instance_remove_event_listener),
                    );

                    *resolved = true;
                    trace!("resolved removeEventListener {}", prop_name);
                } else if prop_name.as_str().eq("dispatchEvent") {
                    trace!("define dispatchEvent");

                    let robj = mozjs::rust::HandleObject::from_marked_location(&obj.get());
                    crate::es_utils::functions::define_native_function(
                        cx,
                        robj,
                        "dispatchEvent",
                        Some(proxy_instance_dispatch_event),
                    );

                    *resolved = true;
                    trace!("resolved dispatchEvent {}", prop_name);
                } else if proxy.properties.contains_key(prop_name.as_str()) {
                    trace!(
                        "define prop for proxy {} for name {}",
                        class_name,
                        prop_name
                    );

                    let n = format!("{}\0", prop_name);

                    // todo move this to es_utils (objects::define_native_getter_setter)
                    let ok = mozjs::jsapi::JS_DefineProperty1(
                        cx,
                        obj,
                        n.as_ptr() as *const libc::c_char,
                        Some(proxy_instance_getter),
                        Some(proxy_instance_setter),
                        (mozjs::jsapi::JSPROP_PERMANENT
                            & mozjs::jsapi::JSPROP_GETTER
                            & mozjs::jsapi::JSPROP_SETTER) as u32,
                    );
                    if !ok {
                        panic!("could not define prop");
                    }

                    *resolved = true;

                    trace!("resolved prop {}", prop_name);
                } else if proxy.methods.contains_key(prop_name.as_str()) {
                    trace!(
                        "define method for proxy {} for name {}",
                        class_name,
                        prop_name
                    );

                    let robj = mozjs::rust::HandleObject::from_marked_location(&obj.get());
                    crate::es_utils::functions::define_native_function(
                        cx,
                        robj,
                        prop_name.as_str(),
                        Some(proxy_instance_method),
                    );

                    *resolved = true;
                    trace!("resolved method {}", prop_name);
                } else if proxy.native_methods.contains_key(prop_name.as_str()) {
                    trace!(
                        "define native method for proxy {} for name {}",
                        class_name,
                        prop_name
                    );

                    let robj = mozjs::rust::HandleObject::from_marked_location(&obj.get());

                    let method: JSNative = proxy
                        .native_methods
                        .get(prop_name.as_str())
                        .cloned()
                        .unwrap();

                    crate::es_utils::functions::define_native_function(
                        cx,
                        robj,
                        prop_name.as_str(),
                        method,
                    );

                    *resolved = true;
                    trace!("resolved native method {}", prop_name);
                }
            }
        });
    }

    true
}

unsafe extern "C" fn proxy_instance_getter(
    cx: *mut JSContext,
    argc: u32,
    vp: *mut mozjs::jsapi::Value,
) -> bool {
    trace!("reflection::getter");

    let args = CallArgs::from_vp(vp, argc);
    let thisv: mozjs::jsapi::Value = *args.thisv();

    if thisv.is_object() {
        if let Some(proxy) = get_proxy_for(cx, thisv.to_object()) {
            let obj_handle = mozjs::rust::HandleObject::from_marked_location(&thisv.to_object());

            trace!("reflection::getter get for cn:{}", proxy.class_name);

            let callee: *mut JSObject = args.callee();
            let prop_name_res = crate::es_utils::objects::get_es_obj_prop_val_as_string(
                cx,
                HandleObject::from_marked_location(&callee),
                "name",
            );
            if let Ok(prop_name) = prop_name_res {
                // lovely the name here is "get [propname]"
                trace!(
                    "reflection::getter get {} for cn:{}",
                    prop_name,
                    proxy.class_name
                );

                // get obj id
                let obj_id = crate::es_utils::objects::get_es_obj_prop_val_as_i32(
                    cx,
                    obj_handle,
                    PROXY_PROP_OBJ_ID,
                );

                trace!(
                    "reflection::getter get {} for cn:{} for obj_id {}",
                    prop_name,
                    proxy.class_name,
                    obj_id
                );

                let p_name = &prop_name[4..];

                if let Some(prop) = proxy.properties.get(p_name) {
                    let js_val_res = prop.0(cx, obj_id);
                    trace!("got val for getter");
                    match js_val_res {
                        Ok(js_val) => {
                            args.rval().set(js_val);
                        }
                        Err(js_err) => {
                            let s = format!("method {} failed\ncaused by: {}\0", p_name, js_err);
                            JS_ReportErrorASCII(cx, s.as_ptr() as *const libc::c_char);
                            return false;
                        }
                    }
                }
            }
        }
    }

    true
}

unsafe extern "C" fn proxy_static_getter(
    cx: *mut JSContext,
    argc: u32,
    vp: *mut mozjs::jsapi::Value,
) -> bool {
    trace!("reflection::static_getter");

    let args = CallArgs::from_vp(vp, argc);
    let thisv: mozjs::jsapi::Value = *args.thisv();

    if thisv.is_object() {
        if let Some(proxy) = get_static_proxy_for(cx, thisv.to_object()) {
            trace!("reflection::static_getter get for cn:{}", proxy.class_name);

            let callee: *mut JSObject = args.callee();
            let prop_name_res = crate::es_utils::objects::get_es_obj_prop_val_as_string(
                cx,
                HandleObject::from_marked_location(&callee),
                "name",
            );
            if let Ok(prop_name) = prop_name_res {
                // lovely the name here is "get [propname]"
                trace!(
                    "reflection::static_getter get {} for cn:{}",
                    prop_name,
                    proxy.class_name
                );

                let p_name = &prop_name[4..];

                if let Some(prop) = proxy.static_properties.get(p_name) {
                    let js_val_res = prop.0(cx);
                    trace!("got val for static_getter");
                    match js_val_res {
                        Ok(js_val) => {
                            args.rval().set(js_val);
                        }
                        Err(js_err) => {
                            let s = format!("getter {} failed\ncaused by: {}\0", p_name, js_err);
                            JS_ReportErrorASCII(cx, s.as_ptr() as *const libc::c_char);
                            return false;
                        }
                    }
                }
            }
        }
    }

    true
}

pub fn get_obj_id_for(cx: *mut JSContext, obj: *mut JSObject) -> i32 {
    let obj_handle = unsafe { mozjs::rust::HandleObject::from_marked_location(&obj) };
    crate::es_utils::objects::get_es_obj_prop_val_as_i32(cx, obj_handle, PROXY_PROP_OBJ_ID)
}

pub fn get_proxy_for(cx: *mut JSContext, obj: *mut JSObject) -> Option<Arc<Proxy>> {
    let obj_handle = unsafe { mozjs::rust::HandleObject::from_marked_location(&obj) };
    let cn_res = crate::es_utils::objects::get_es_obj_prop_val_as_string(
        cx,
        obj_handle,
        PROXY_PROP_CLASS_NAME,
    );
    if let Ok(class_name) = cn_res {
        return PROXIES.with(|proxies_rc| {
            let proxies = &*proxies_rc.borrow();
            proxies.get(class_name.as_str()).cloned()
        });
    }

    None
}

fn get_static_proxy_for(cx: *mut JSContext, obj: *mut JSObject) -> Option<Arc<Proxy>> {
    let obj_handle = unsafe { mozjs::rust::HandleObject::from_marked_location(&obj) };
    let cn_res = crate::es_utils::objects::get_es_obj_prop_val_as_string(
        cx,
        obj_handle,
        PROXY_PROP_CLASS_NAME,
    );
    if let Ok(class_name) = cn_res {
        return PROXIES.with(|proxies_rc| {
            let proxies = &*proxies_rc.borrow();
            proxies.get(class_name.as_str()).cloned()
        });
    }

    None
}

unsafe extern "C" fn proxy_instance_setter(
    cx: *mut JSContext,
    argc: u32,
    vp: *mut mozjs::jsapi::Value,
) -> bool {
    trace!("reflection::setter");

    let args = CallArgs::from_vp(vp, argc);
    let this_val: mozjs::jsapi::Value = *args.thisv();

    if this_val.is_object() {
        if let Some(proxy) = get_proxy_for(cx, this_val.to_object()) {
            trace!("reflection::setter get for cn:{}", &proxy.class_name);

            let callee: *mut JSObject = args.callee();
            let prop_name_res = crate::es_utils::objects::get_es_obj_prop_val_as_string(
                cx,
                HandleObject::from_marked_location(&callee),
                "name",
            );
            if let Ok(prop_name) = prop_name_res {
                // lovely the name here is "set [propname]"
                trace!("reflection::setter set {}", prop_name);

                // get obj id
                let obj_id = get_obj_id_for(cx, this_val.to_object());

                trace!(
                    "reflection::setter set {} for for obj_id {}",
                    prop_name,
                    obj_id
                );

                // strip "set " from propname
                let p_name = &prop_name[4..];

                if let Some(prop) = proxy.properties.get(p_name) {
                    let val = HandleValue::from_marked_location(&args.index(0).get());

                    trace!("reflection::setter setting val");
                    let js_val_res = prop.1(cx, obj_id, val);
                    if let Err(js_err) = js_val_res {
                        let s = format!("setter {} failed\ncaused by: {}\0", p_name, js_err);
                        JS_ReportErrorASCII(cx, s.as_ptr() as *const libc::c_char);
                        return false;
                    }
                }
            }
        }
    }

    true
}

unsafe extern "C" fn proxy_static_setter(
    cx: *mut JSContext,
    argc: u32,
    vp: *mut mozjs::jsapi::Value,
) -> bool {
    trace!("reflection::static_setter");

    let args = CallArgs::from_vp(vp, argc);
    let this_val: mozjs::jsapi::Value = *args.thisv();

    if this_val.is_object() {
        if let Some(proxy) = get_static_proxy_for(cx, this_val.to_object()) {
            trace!("reflection::static_setter get for cn:{}", &proxy.class_name);

            let callee: *mut JSObject = args.callee();
            let prop_name_res = crate::es_utils::objects::get_es_obj_prop_val_as_string(
                cx,
                HandleObject::from_marked_location(&callee),
                "name",
            );
            if let Ok(prop_name) = prop_name_res {
                // lovely the name here is "set [propname]"
                trace!("reflection::static_setter set {}", prop_name);

                // strip "set " from propname
                let p_name = &prop_name[4..];

                if let Some(prop) = proxy.static_properties.get(p_name) {
                    let val = HandleValue::from_marked_location(&args.index(0).get());

                    trace!("reflection::static_setter setting val");
                    let js_val_res = prop.1(cx, val);
                    if let Err(js_err) = js_val_res {
                        let s = format!("setter {} failed\ncaused by: {}\0", p_name, js_err);
                        JS_ReportErrorASCII(cx, s.as_ptr() as *const libc::c_char);
                        return false;
                    }
                }
            }
        }
    }

    true
}

unsafe extern "C" fn proxy_static_add_event_listener(
    cx: *mut JSContext,
    argc: u32,
    vp: *mut mozjs::jsapi::Value,
) -> bool {
    trace!("add_static_event_listener");

    if argc >= 2 {
        let args = CallArgs::from_vp(vp, argc);
        let type_handle_val = args.index(0);
        let listener_handle_val = *args.index(1);

        let listener_obj: *mut JSObject = listener_handle_val.to_object();

        let listener_epr = EsPersistentRooted::new_from_obj(cx, listener_obj);
        let type_str = crate::es_utils::es_value_to_str(cx, *type_handle_val)
            .ok()
            .unwrap();

        let thisv: mozjs::jsapi::Value = *args.thisv();

        if let Some(proxy) = get_static_proxy_for(cx, thisv.to_object()) {
            if proxy.static_events.contains(&type_str.as_str()) {
                // we need this so we can get a &'static str
                let type_str = &&(*(*proxy.static_events.get(type_str.as_str()).unwrap()));

                let obj_map = &mut *proxy.static_event_listeners.borrow_mut();

                if !obj_map.contains_key(type_str) {
                    obj_map.insert(type_str, vec![]);
                }

                let listener_vec = obj_map.get_mut(type_str).unwrap();
                listener_vec.push(listener_epr);
            } else {
                trace!(
                    "add_static_event_listener -> static event not defined: {}",
                    type_str
                );
            }
        } else {
            trace!("add_static_event_listener -> no proxy found for obj");
        }
    }

    true
}

unsafe extern "C" fn proxy_static_remove_event_listener(
    cx: *mut JSContext,
    argc: u32,
    vp: *mut mozjs::jsapi::Value,
) -> bool {
    trace!("remove_static_event_listener");
    if argc >= 2 {
        let args = CallArgs::from_vp(vp, argc);
        let type_handle_val = args.index(0);
        let listener_handle_val = *args.index(1);

        let listener_obj: *mut JSObject = listener_handle_val.to_object();

        let type_str = crate::es_utils::es_value_to_str(cx, *type_handle_val)
            .ok()
            .unwrap();

        let thisv: mozjs::jsapi::Value = *args.thisv();

        if let Some(proxy) = get_static_proxy_for(cx, thisv.to_object()) {
            if proxy.static_events.contains(&type_str.as_str()) {
                // we need this so we can get a &'static str
                let type_str = &&(*(*proxy.static_events.get(type_str.as_str()).unwrap()));

                let obj_map = &mut *proxy.static_event_listeners.borrow_mut();

                if obj_map.contains_key(type_str) {
                    let listener_vec = obj_map.get_mut(type_str).unwrap();
                    for x in 0..listener_vec.len() {
                        let epr = listener_vec.get(x).unwrap();
                        if epr.get() == listener_obj {
                            trace!("remove static event listener for {}", type_str);
                            listener_vec.remove(x);
                            break;
                        }
                    }
                }
            }
        }
    }
    true
}

unsafe extern "C" fn proxy_static_dispatch_event(
    cx: *mut JSContext,
    argc: u32,
    vp: *mut mozjs::jsapi::Value,
) -> bool {
    trace!("dispatch_static_event");

    if argc >= 2 {
        let args = CallArgs::from_vp(vp, argc);
        let type_handle_val = args.index(0);
        let evt_obj_handle_val = args.index(1);

        let type_str = crate::es_utils::es_value_to_str(cx, *type_handle_val)
            .ok()
            .unwrap();

        let thisv: mozjs::jsapi::Value = *args.thisv();

        if let Some(proxy) = get_static_proxy_for(cx, thisv.to_object()) {
            if proxy.static_events.contains(&type_str.as_str()) {
                let type_str = &&(*(*proxy.static_events.get(type_str.as_str()).unwrap()));

                dispatch_static_event_for_proxy(cx, proxy.borrow(), type_str, evt_obj_handle_val);
            }
        }
    }
    true
}

unsafe extern "C" fn proxy_instance_add_event_listener(
    cx: *mut JSContext,
    argc: u32,
    vp: *mut mozjs::jsapi::Value,
) -> bool {
    trace!("add_event_listener");

    if argc >= 2 {
        let args = CallArgs::from_vp(vp, argc);
        let type_handle_val = args.index(0);
        let listener_handle_val = *args.index(1);

        let listener_obj: *mut JSObject = listener_handle_val.to_object();

        let listener_epr = EsPersistentRooted::new_from_obj(cx, listener_obj);
        let type_str = crate::es_utils::es_value_to_str(cx, *type_handle_val)
            .ok()
            .unwrap();

        let thisv: mozjs::jsapi::Value = *args.thisv();

        let obj_id = get_obj_id_for(cx, thisv.to_object());

        if let Some(proxy) = get_proxy_for(cx, thisv.to_object()) {
            if proxy.events.contains(&type_str.as_str()) {
                // we need this so we can get a &'static str
                let type_str = &&(*(*proxy.events.get(type_str.as_str()).unwrap()));

                let pel = &mut *proxy.event_listeners.borrow_mut();
                pel.entry(obj_id).or_insert_with(HashMap::new);
                let obj_map = pel.get_mut(&obj_id).unwrap();

                if !obj_map.contains_key(type_str) {
                    obj_map.insert(type_str, vec![]);
                }

                let listener_vec = obj_map.get_mut(type_str).unwrap();
                listener_vec.push(listener_epr);
            } else {
                trace!("add_event_listener -> event not defined: {}", type_str);
            }
        }
    }

    true
}

unsafe extern "C" fn proxy_instance_remove_event_listener(
    cx: *mut JSContext,
    argc: u32,
    vp: *mut mozjs::jsapi::Value,
) -> bool {
    trace!("remove_event_listener");
    if argc >= 2 {
        let args = CallArgs::from_vp(vp, argc);
        let type_handle_val = args.index(0);
        let listener_handle_val = *args.index(1);

        let listener_obj: *mut JSObject = listener_handle_val.to_object();

        let type_str = crate::es_utils::es_value_to_str(cx, *type_handle_val)
            .ok()
            .unwrap();

        let thisv: mozjs::jsapi::Value = *args.thisv();

        let obj_id = get_obj_id_for(cx, thisv.to_object());

        if let Some(proxy) = get_proxy_for(cx, thisv.to_object()) {
            if proxy.events.contains(&type_str.as_str()) {
                // we need this so we can get a &'static str
                let type_str = &&(*(*proxy.events.get(type_str.as_str()).unwrap()));

                let pel = &mut *proxy.event_listeners.borrow_mut();

                if pel.contains_key(&obj_id) {
                    let obj_map = pel.get_mut(&obj_id).unwrap();

                    if obj_map.contains_key(type_str) {
                        let listener_vec = obj_map.get_mut(type_str).unwrap();
                        for x in 0..listener_vec.len() {
                            let epr = listener_vec.get(x).unwrap();
                            if epr.get() == listener_obj {
                                trace!("remove event listener for {}", type_str);
                                listener_vec.remove(x);
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    true
}

unsafe extern "C" fn proxy_instance_dispatch_event(
    cx: *mut JSContext,
    argc: u32,
    vp: *mut mozjs::jsapi::Value,
) -> bool {
    trace!("dispatch_event");

    if argc >= 2 {
        let args = CallArgs::from_vp(vp, argc);
        let type_handle_val = args.index(0);
        let evt_obj_handle_val = args.index(1);

        let type_str = crate::es_utils::es_value_to_str(cx, *type_handle_val)
            .ok()
            .unwrap();

        let thisv: mozjs::jsapi::Value = *args.thisv();

        let obj_id = get_obj_id_for(cx, thisv.to_object());

        if let Some(proxy) = get_proxy_for(cx, thisv.to_object()) {
            if proxy.events.contains(&type_str.as_str()) {
                let type_str = &&(*(*proxy.events.get(type_str.as_str()).unwrap()));

                dispatch_event_for_proxy(cx, proxy.borrow(), obj_id, type_str, evt_obj_handle_val);
            }
        }
    }
    true
}

// proxy can call this from Proxy::dispatch_event with esvf.to_es_val()
fn dispatch_event_for_proxy(
    cx: *mut JSContext,
    proxy: &Proxy,
    obj_id: i32,
    evt_type: &str,
    evt_obj: mozjs::jsapi::HandleValue,
) {
    let pel = &*proxy.event_listeners.borrow();
    if let Some(obj_map) = pel.get(&obj_id) {
        if let Some(listener_vec) = obj_map.get(evt_type) {
            rooted!(in (cx) let mut ret_val = UndefinedValue());
            // todo this_obj should be the proxy obj..
            rooted!(in (cx) let this_obj = NullValue().to_object_or_null());
            // since evt_obj is already rooted here we don;t need the auto_root macro, we can just use call_method_value()

            for listener_epr in listener_vec {
                let mut args_vec = vec![];
                args_vec.push(*evt_obj);
                let func_obj = listener_epr.get();
                // todo why do we only have a call_method by val and not by HandleObject?
                // the whole rooting func here could be avoided
                rooted!(in (cx) let function_val = ObjectValue(func_obj));
                crate::es_utils::functions::call_method_value(
                    cx,
                    this_obj.handle(),
                    function_val.handle(),
                    args_vec,
                    ret_val.handle_mut(),
                )
                .ok()
                .unwrap();
            }
        }
    }
}

fn dispatch_static_event_for_proxy(
    cx: *mut JSContext,
    proxy: &Proxy,
    evt_type: &str,
    evt_obj: mozjs::jsapi::HandleValue,
) {
    let obj_map = &*proxy.static_event_listeners.borrow();

    if let Some(listener_vec) = obj_map.get(evt_type) {
        rooted!(in (cx) let mut ret_val = UndefinedValue());
        // todo this_obj should be the proxy obj..
        rooted!(in (cx) let this_obj = NullValue().to_object_or_null());
        // since evt_obj is already rooted here we don;t need the auto_root macro, we can just use call_method_value()

        for listener_epr in listener_vec {
            let mut args_vec = vec![];
            args_vec.push(*evt_obj);
            let func_obj = listener_epr.get();
            // todo why do we only have a call_method by val and not by HandleObject?
            // the whole rooting func here could be avoided
            rooted!(in (cx) let function_val = ObjectValue(func_obj));
            crate::es_utils::functions::call_method_value(
                cx,
                this_obj.handle(),
                function_val.handle(),
                args_vec,
                ret_val.handle_mut(),
            )
            .ok()
            .unwrap();
        }
    }
}

unsafe extern "C" fn proxy_instance_method(
    cx: *mut JSContext,
    argc: u32,
    vp: *mut mozjs::jsapi::Value,
) -> bool {
    trace!("reflection::method");

    let args = CallArgs::from_vp(vp, argc);
    let thisv: mozjs::jsapi::Value = *args.thisv();

    if thisv.is_object() {
        if let Some(proxy) = get_proxy_for(cx, thisv.to_object()) {
            trace!("reflection::method for cn:{}", &proxy.class_name);

            let callee: *mut JSObject = args.callee();
            let prop_name_res = crate::es_utils::objects::get_es_obj_prop_val_as_string(
                cx,
                HandleObject::from_marked_location(&callee),
                "name",
            );
            if let Ok(prop_name) = prop_name_res {
                // lovely the name here is "get [propname]"
                trace!("reflection::method {}", prop_name);

                // get obj id
                let obj_id = get_obj_id_for(cx, thisv.to_object());

                trace!("reflection::method {} for for obj_id {}", prop_name, obj_id);

                let p_name = prop_name.as_str();

                if let Some(prop) = proxy.methods.get(p_name) {
                    trace!("got method for method");

                    let mut args_vec = vec![];
                    for x in 0..args.argc_ {
                        args_vec.push(HandleValue::from_marked_location(&*args.get(x)));
                    }

                    let js_val_res = prop(cx, obj_id, &args_vec);
                    match js_val_res {
                        Ok(js_val) => {
                            args.rval().set(js_val);
                        }
                        Err(js_err) => {
                            let s = format!("method {} failed\ncaused by: {}\0", p_name, js_err);
                            JS_ReportErrorASCII(cx, s.as_ptr() as *const libc::c_char);
                            return false;
                        }
                    }
                }
            }
        }
    }

    true
}

unsafe extern "C" fn proxy_static_method(
    cx: *mut JSContext,
    argc: u32,
    vp: *mut mozjs::jsapi::Value,
) -> bool {
    trace!("reflection::static_method");

    let args = CallArgs::from_vp(vp, argc);
    let thisv: mozjs::jsapi::Value = *args.thisv();

    if thisv.is_object() {
        if let Some(proxy) = get_static_proxy_for(cx, thisv.to_object()) {
            trace!("reflection::static_method for cn:{}", &proxy.class_name);

            let callee: *mut JSObject = args.callee();
            let prop_name_res = crate::es_utils::objects::get_es_obj_prop_val_as_string(
                cx,
                HandleObject::from_marked_location(&callee),
                "name",
            );
            if let Ok(prop_name) = prop_name_res {
                // lovely the name here is "get [propname]"
                trace!("reflection::static_method {}", prop_name);

                let p_name = prop_name.as_str();

                if let Some(prop) = proxy.static_methods.get(p_name) {
                    trace!("got method for static_method");

                    let mut args_vec = vec![];
                    for x in 0..args.argc_ {
                        args_vec.push(HandleValue::from_marked_location(&*args.get(x)));
                    }

                    let js_val_res = prop(cx, &args_vec);
                    match js_val_res {
                        Ok(js_val) => {
                            args.rval().set(js_val);
                        }
                        Err(js_err) => {
                            let s =
                                format!("static method {} failed\ncaused by: {}\0", p_name, js_err);
                            JS_ReportErrorASCII(cx, s.as_ptr() as *const libc::c_char);
                            return false;
                        }
                    }
                }
            }
        }
    }

    true
}

unsafe extern "C" fn proxy_instance_finalize(_fop: *mut JSFreeOp, object: *mut JSObject) {
    trace!("reflection::finalize");

    let ptr_usize = object as usize;
    let id_opt = PROXY_INSTANCE_IDS.with(|piid_rc| {
        let piid = &mut *piid_rc.borrow_mut();
        piid.remove(&ptr_usize)
    });

    if let Some(id) = id_opt {
        let cn = PROXY_INSTANCE_CLASSNAMES
            .with(|piid_rc| {
                let piid = &mut *piid_rc.borrow_mut();
                piid.remove(&id)
            })
            .unwrap();

        trace!("finalize id {} of type {}", id, cn);
        if let Some(proxy) = get_proxy(cn.as_str()) {
            if let Some(finalizer) = &proxy.finalizer {
                finalizer(&id);
            }

            // clear event listeners
            let pel = &mut *proxy.event_listeners.borrow_mut();
            pel.remove(&id);
        }
    }
}

const PROXY_PROP_CLASS_NAME: &str = "__proxy_class_name__";
const PROXY_PROP_OBJ_ID: &str = "__proxy_obj_id__";

unsafe extern "C" fn proxy_construct(
    cx: *mut JSContext,
    argc: u32,
    vp: *mut mozjs::jsapi::Value,
) -> bool {
    trace!("reflection::construct");

    let args = CallArgs::from_vp(vp, argc);

    rooted!(in (cx) let constructor_root = args.calleev().to_object());

    let class_name = crate::es_utils::objects::get_es_obj_prop_val_as_string(
        cx,
        constructor_root.handle(),
        PROXY_PROP_CLASS_NAME,
    )
    .ok()
    .unwrap();
    trace!("reflection::construct cn={}", class_name);

    if let Some(proxy) = get_proxy(class_name.as_str()) {
        trace!("constructing proxy {}", class_name);
        if let Some(constructor) = &proxy.constructor {
            trace!("constructing proxy constructor {}", class_name);

            let mut args_vec = vec![];
            for x in 0..args.argc_ {
                args_vec.push(HandleValue::from_marked_location(&*args.get(x)));
            }

            let obj_id_res = constructor(cx, &args_vec);

            if obj_id_res.is_ok() {
                let obj_id = obj_id_res.ok().unwrap();
                let ret: *mut JSObject = mozjs::jsapi::JS_NewObject(cx, &ES_PROXY_CLASS);

                rooted!(in (cx) let ret_root = ret);
                rooted!(in (cx) let pname_root = crate::es_utils::new_es_value_from_str(cx, &class_name));
                rooted!(in (cx) let obj_id_root = mozjs::jsval::Int32Value(obj_id));
                crate::es_utils::objects::set_es_obj_prop_val_permanent(
                    cx,
                    ret_root.handle(),
                    PROXY_PROP_CLASS_NAME,
                    pname_root.handle(),
                );
                crate::es_utils::objects::set_es_obj_prop_val_permanent(
                    cx,
                    ret_root.handle(),
                    PROXY_PROP_OBJ_ID,
                    obj_id_root.handle(),
                );

                PROXY_INSTANCE_IDS.with(|piid_rc| {
                    let piid = &mut *piid_rc.borrow_mut();
                    piid.insert(ret as usize, obj_id.clone());
                });

                PROXY_INSTANCE_CLASSNAMES.with(|piid_rc| {
                    let piid = &mut *piid_rc.borrow_mut();
                    piid.insert(obj_id.clone(), class_name.clone());
                });

                args.rval().set(ObjectValue(ret));

                return true;
            } else {
                JS_ReportErrorASCII(cx, b"constructor failed\0".as_ptr() as *const libc::c_char);

                return false;
            }
        }
    }

    JS_ReportErrorASCII(cx, b"no such class found\0".as_ptr() as *const libc::c_char);

    false
}
