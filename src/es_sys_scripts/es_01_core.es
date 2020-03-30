

this.esses = new (class Esses {

    constructor() {

        this._next_id = 0;
        this._cleanup_jobs = [];
        this._registered_promises = new Map();

    }

    /**
    * generate a new id and resolve values with that id later
    */
    registerPromiseForResolutionInRust(prom) {
        if (typeof prom === 'object' && prom instanceof Promise) {
            let id = this.next_id();
            // then and catch are registered async to prevent direct resolution without id being registered in rust
            // we also store the promise in a Map so it is not garbage collected
            this._registered_promises.set(id, prom);

            setImmediate(function(prom, id) {
                esses.register_waitfor_promise(prom, id);
            }, prom, id);
            return id;
        } else {
            throw Error("value pass to registerPromiseForResolutionInRust was not a Promise [" + typeof prom + "]");
        }
    }

    next_id() {
        return this._next_id++;
    }

    /**
    * currently this runs in the same threadpool as all jobs for this runtime but in future this will become a multithreaded pool
    * @returns {Promise}
    */
    invoke_rust_op(name, ...args) {

        return new Promise((resolve, reject) => {
            try {
                let res = this.invoke_rust_op_sync(name, args);
                resolve(res);
            } catch(ex) {
                reject(ex);
            }
        });

    }

    /**
    * @returns {Void}
    */
    invoke_rust_op_void(name, ...args) {

        setImmediate(() => {
            this.invoke_rust_op_sync(name, args);
        });

    }

    /**
    * @returns {Any}
    */
    invoke_rust_op_sync(name, ...args) {

        console.log("invoke_rust_op_sync %s ", name);
        try {
            let rust_result = __invoke_rust_op(name, ...args);
            return rust_result;
        } catch(ex) {
            console.error("invoke_rust_op_sync %s failed with %s", name, "" + ex);
        }

    }

    register_waitfor_promise(val, man_obj_id) {

        if (val instanceof Promise) {
            val.then((result) => {
                console.trace('resolving esvf from es to {}', result);
                esses.invoke_rust_op_sync('resolve_waiting_esvf_future', man_obj_id, result);
            });
            val.catch((ex) => {
                console.trace('rejecting esvf from es to {}', ex);
                esses.invoke_rust_op_sync('reject_waiting_esvf_future', man_obj_id, ex);
            });
            val.finally(() => {
                esses._registered_promises.remove(id);
            });
        } else {
            let t = "" + val;
            if (val && val.constructor) {
                t = val.constructor.name;
            } else if (val){
                t = JSON.stringify(val);
            }
            throw Error("_register_waitfor_promise_ managed obj was not a promise: " + t);
        }
    }

    /**
    * add a job todo when cleanup is called from rust
    */
    add_cleanup_job(job) {
        if (!job instanceof Function) {
            throw Error("job was not a function");
        }
        this._cleanup_jobs.push(job);
    }

    /**
    * called from rust before running cleanup
    */
    cleanup() {
        console.debug("running esses.cleanup()");
        for (let job of this._cleanup_jobs) {
            job();
        }
    }


})();

