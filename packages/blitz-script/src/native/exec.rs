use super::syntax::*;
use super::*;
use std::rc::{Rc, Weak};

const MAX_SOURCE_BYTES: usize = 1_048_576;
const MAX_CALL_DEPTH: usize = 256;
const MAX_AWAIT_POLLS: usize = 1_024;
pub(super) const MAX_ARRAY_LENGTH: usize = 65_536;
const MAX_CALL_LABEL: usize = 96;

fn append_call_label(label: &mut String, suffix: &str) {
    if label.len() >= MAX_CALL_LABEL {
        return;
    }
    let remaining = MAX_CALL_LABEL - label.len();
    label.push_str(&suffix.chars().take(remaining).collect::<String>());
}

fn call_label(expression: &Expr) -> String {
    match expression {
        Expr::Ident(name) => name.chars().take(MAX_CALL_LABEL).collect(),
        Expr::Member(object, key) => {
            let mut label = call_label(object);
            match key.as_ref() {
                Expr::Literal(Literal::String(key)) => {
                    append_call_label(&mut label, ".");
                    append_call_label(
                        &mut label,
                        &String::from_utf16_lossy(&key[..key.len().min(MAX_CALL_LABEL)]),
                    );
                }
                _ => append_call_label(&mut label, "[<computed>]"),
            }
            label
        }
        Expr::Chain(base, parts) => {
            let mut label = call_label(base);
            for part in parts {
                match part {
                    ChainPart::Member(Expr::Literal(Literal::String(key)), _) => {
                        append_call_label(&mut label, ".");
                        append_call_label(
                            &mut label,
                            &String::from_utf16_lossy(&key[..key.len().min(MAX_CALL_LABEL)]),
                        );
                    }
                    ChainPart::Member(_, _) => append_call_label(&mut label, "[<computed>]"),
                    ChainPart::Call(_, _) => append_call_label(&mut label, "()"),
                }
            }
            label
        }
        Expr::Function(function) => function
            .name
            .as_deref()
            .map(|name| name.chars().take(MAX_CALL_LABEL).collect())
            .unwrap_or_else(|| "<function>".into()),
        _ => "<expression>".into(),
    }
}

fn class_fields(metadata: &Value) -> Vec<(String, Option<Expr>)> {
    metadata
        .as_object()
        .and_then(|object| object.borrow().payload.clone())
        .and_then(|payload| {
            payload
                .downcast_ref::<Vec<(String, Option<Expr>)>>()
                .cloned()
        })
        .unwrap_or_default()
}

#[derive(Clone)]
enum Flow {
    Normal,
    Return(Value),
    Break,
    Continue,
}

type ExecResult = Result<Flow>;

impl<B: RuntimeBridge> Guest<B> {
    fn error_value(&self, error: &Error) -> Value {
        let value = error.value();
        if error.thrown.is_none() {
            let constructor = match error.name.as_str() {
                "TypeError" | "RangeError" | "ReferenceError" | "SyntaxError" => &error.name,
                _ => "Error",
            };
            let prototype = self.global.get(constructor).get("prototype");
            if let Some(object) = value.as_object() {
                object.borrow_mut().prototype = Some(prototype);
            }
        }
        value
    }

    /// Parse and execute one source unit in this guest's persistent global scope.
    pub fn evaluate(&mut self, doc: &mut BaseDocument, source: &str) -> Result<Value> {
        let _scope = super::value::HeapScope::enter(&self.heap);
        if self.disposed {
            return Err(Error::new("InvalidStateError", "guest has been disposed"));
        }
        if source.len() > MAX_SOURCE_BYTES {
            return Err(Error::syntax("source exceeds the execution size limit"));
        }
        let program = parse(source).map_err(Error::syntax)?;
        let env = Rc::clone(&self.env);
        self.hoist(doc, &program, &env)?;
        if let Some((Stmt::Expr(last), prefix)) = program.split_last() {
            match self.exec_statements(doc, prefix, &env)? {
                Flow::Normal => return self.eval_expr(doc, last, &env),
                Flow::Return(value) => return Ok(value),
                Flow::Break | Flow::Continue => {
                    return Err(Error::syntax("control statement escaped the program"));
                }
            }
        }
        match self.exec_statements(doc, &program, &env)? {
            Flow::Return(value) => Ok(value),
            Flow::Normal | Flow::Break | Flow::Continue => Ok(Value::Undefined),
        }
    }

    /// Invoke a retained parsed function with its captured lexical environment.
    pub fn call_function(
        &mut self,
        doc: &mut BaseDocument,
        function: Rc<Function>,
        this: Value,
        args: Vec<Value>,
        construct: bool,
    ) -> Result<Value> {
        if function.asynchronous && construct {
            return Err(Error::type_error("async functions are not constructors"));
        }
        let asynchronous = function.asynchronous;
        let result = if self.depth >= MAX_CALL_DEPTH {
            Err(Error::new(
                "RangeError",
                "maximum guest call depth exceeded",
            ))
        } else {
            self.depth += 1;
            let result = self.call_function_inner(doc, function, this, args, construct);
            self.depth -= 1;
            result
        };
        if !asynchronous {
            return result;
        }
        let promise = Value::pending_promise();
        match result {
            Ok(value) => self.resolve_promise(doc, &promise, value, false)?,
            Err(error) => {
                let reason = self.error_value(&error);
                self.resolve_promise(doc, &promise, reason, true)?;
            }
        }
        Ok(promise)
    }

    fn call_function_inner(
        &mut self,
        doc: &mut BaseDocument,
        function: Rc<Function>,
        this: Value,
        args: Vec<Value>,
        construct: bool,
    ) -> Result<Value> {
        let derived_constructor = construct
            && function
                .env
                .borrow()
                .bindings
                .contains_key("\0derived_constructor");
        let receiver = if derived_constructor {
            Value::Undefined
        } else if function.arrow && construct {
            return Err(Error::type_error("arrow functions are not constructors"));
        } else if function.arrow {
            function.this.clone()
        } else if construct {
            let instance = Value::object("Object");
            let prototype = Self::lookup(&function.env, "\0self")
                .map(|constructor| constructor.get("prototype"))
                .unwrap_or(Value::Undefined);
            if let Some(object) = instance.as_object() {
                if matches!(prototype, Value::Object(_)) {
                    object.borrow_mut().prototype = Some(prototype);
                }
            }
            instance
        } else {
            this
        };

        let env = Environment::new(Some(Rc::clone(&function.env)));
        env.borrow_mut()
            .bindings
            .insert("this".into(), receiver.clone());
        if derived_constructor {
            env.borrow_mut()
                .bindings
                .insert("\0derived_constructor_call".into(), Value::Bool(true));
        }
        if !function.arrow {
            let arguments = Value::object("Arguments");
            if let Some(object) = arguments.as_object() {
                let mut object = object.borrow_mut();
                object.indexed = args.clone();
                object.props.insert(
                    "length".into(),
                    Property {
                        value: Value::Number(args.len() as f64),
                        get: None,
                        set: None,
                        enumerable: false,
                    },
                );
            }
            env.borrow_mut()
                .bindings
                .insert("arguments".into(), arguments);
        }
        for (index, pattern) in function.params.iter().enumerate() {
            let value = if matches!(pattern, Pattern::Rest(_)) {
                Value::array(
                    args.iter()
                        .skip(index)
                        .take(MAX_ARRAY_LENGTH)
                        .cloned()
                        .collect(),
                )
            } else {
                args.get(index).cloned().unwrap_or(Value::Undefined)
            };
            self.bind_pattern(doc, pattern, value, &env)?;
        }
        if construct && !derived_constructor {
            let fields = function
                .env
                .borrow()
                .bindings
                .get("\0instance_fields")
                .map(class_fields)
                .unwrap_or_default();
            let field_env = Environment::new(Some(Rc::clone(&function.env)));
            field_env
                .borrow_mut()
                .bindings
                .insert("this".into(), receiver.clone());
            for (name, initial) in fields {
                let value = if let Some(initial) = initial {
                    self.eval_expr(doc, &initial, &field_env)?
                } else {
                    Value::Undefined
                };
                self.set_property(doc, &receiver, &name, value)?;
            }
        }
        self.hoist(doc, &function.body, &env)?;
        let returned = match self.exec_statements(doc, &function.body, &env)? {
            Flow::Return(value) => value,
            Flow::Normal | Flow::Break | Flow::Continue => Value::Undefined,
        };
        if !construct {
            return Ok(returned);
        }
        if matches!(returned, Value::Object(_)) {
            return Ok(returned);
        }
        if derived_constructor {
            let initialized = Self::lookup(&env, "this").unwrap_or(Value::Undefined);
            if matches!(initialized, Value::Object(_)) {
                Ok(initialized)
            } else {
                Err(Error::type_error(
                    "derived constructor must call super or return an object",
                ))
            }
        } else {
            Ok(receiver)
        }
    }

    /// Resolve a settled promise or drive real bridge/jobs work for a pending one.
    pub fn await_value(&mut self, doc: &mut BaseDocument, value: Value) -> Result<Value> {
        if value.kind() != "Promise" {
            self.resume_after_await()?;
            return Ok(value);
        }
        for _ in 0..MAX_AWAIT_POLLS {
            let state = value.get("state").number_value() as i32;
            if state == 1 {
                self.resume_after_await()?;
                return Ok(value.get("value"));
            }
            if state == 2 {
                self.resume_after_await()?;
                let reason = value.get("value");
                return Err(Error::thrown(reason));
            }

            let host_jobs = self.bridge.poll(doc, None, true)?;
            self.jobs.extend(host_jobs);
            let did_work = self.drain_one_job(doc)?;
            if value.get("state").number_value() != 0.0 {
                continue;
            }
            if !did_work && self.bridge.pending_task_count() == 0 && self.jobs.is_empty() {
                return Err(Error::new(
                    "InvalidStateError",
                    "promise is pending and no host work can resolve it",
                ));
            }
            self.tick()?;
            self.bridge.wait_for_work()?;
        }
        Err(Error::new(
            "TimeoutError",
            "promise did not settle within the bounded poll budget",
        ))
    }

    /// Test prototype identity for parsed functions and native constructors.
    pub fn instance_of(
        &mut self,
        doc: &mut BaseDocument,
        value: &Value,
        constructor: &Value,
    ) -> Result<bool> {
        if !matches!(value, Value::Object(_)) {
            return Ok(false);
        }
        let is_function = constructor
            .as_object()
            .is_some_and(|object| object.borrow().function.is_some())
            || super::proxy::is_callable(constructor);
        let is_native = matches!(constructor.get("__native"), Value::String(_));
        if !is_function && !is_native {
            return Err(Error::type_error(
                "right-hand side of instanceof is not callable",
            ));
        }
        let target = self.get_property(doc, constructor, "prototype")?;
        if !matches!(target, Value::Object(_)) {
            return Err(Error::type_error(
                "right-hand side of instanceof is not callable",
            ));
        }
        let mut cursor = value
            .as_object()
            .and_then(|object| object.borrow().prototype.clone());
        let mut remaining = MAX_CALL_DEPTH * 4;
        while let Some(prototype) = cursor {
            if prototype.same(&target) {
                return Ok(true);
            }
            if remaining == 0 {
                return Err(Error::new("RangeError", "prototype chain exceeds limit"));
            }
            remaining -= 1;
            cursor = prototype
                .as_object()
                .and_then(|object| object.borrow().prototype.clone());
        }
        Ok(false)
    }

    fn lookup(env: &Env, name: &str) -> Option<Value> {
        let (value, parent) = {
            let scope = env.borrow();
            (scope.bindings.get(name).cloned(), scope.parent.clone())
        };
        value
            .and_then(|value| {
                if value.kind() == "FunctionSelfRef" {
                    value
                        .as_object()
                        .and_then(|object| {
                            object
                                .borrow()
                                .payload
                                .as_ref()
                                .and_then(|payload| {
                                    payload.downcast_ref::<Weak<std::cell::RefCell<Object>>>()
                                })
                                .and_then(Weak::upgrade)
                        })
                        .map(Value::Object)
                } else {
                    Some(value)
                }
            })
            .or_else(|| parent.and_then(|parent| Self::lookup(&parent, name)))
    }

    fn assign(env: &Env, name: &str, value: Value) -> bool {
        let parent = {
            let mut scope = env.borrow_mut();
            if scope.bindings.contains_key(name) {
                scope.bindings.insert(name.to_owned(), value);
                return true;
            }
            scope.parent.clone()
        };
        parent.is_some_and(|parent| Self::assign(&parent, name, value))
    }

    fn declare(&self, env: &Env, name: &str, value: Value) {
        env.borrow_mut().bindings.insert(name.to_owned(), value);
    }

    fn hoist(&mut self, doc: &mut BaseDocument, statements: &[Stmt], env: &Env) -> Result<()> {
        for statement in statements {
            match statement {
                Stmt::FunctionDecl(name, function) => {
                    let value = self.make_function(function.clone(), Rc::clone(env));
                    self.declare(env, name, value);
                }
                Stmt::Var(kind, declarations) => {
                    if !matches!(kind, VarKind::Var) {
                        for (pattern, _) in declarations {
                            self.declare_pattern_names(pattern, env);
                        }
                    }
                }
                Stmt::ClassDecl(name, class) => {
                    let value = self.eval_class(doc, class, env)?;
                    self.declare(env, name, value);
                }
                _ => {}
            }
        }
        self.hoist_vars(statements, env)
    }

    fn hoist_vars(&self, statements: &[Stmt], env: &Env) -> Result<()> {
        for statement in statements {
            match statement {
                Stmt::Var(VarKind::Var, declarations) => {
                    for (pattern, _) in declarations {
                        self.declare_pattern_names(pattern, env);
                    }
                }
                Stmt::Block(body) => self.hoist_vars(body, env)?,
                Stmt::If(_, yes, no) => {
                    self.hoist_vars(std::slice::from_ref(yes), env)?;
                    if let Some(no) = no {
                        self.hoist_vars(std::slice::from_ref(no), env)?;
                    }
                }
                Stmt::While(_, body) | Stmt::DoWhile(body, _) => {
                    self.hoist_vars(std::slice::from_ref(body), env)?;
                }
                Stmt::For { init, body, .. } => {
                    if let Some(init) = init {
                        self.hoist_vars(std::slice::from_ref(init), env)?;
                    }
                    self.hoist_vars(std::slice::from_ref(body), env)?;
                }
                Stmt::ForEach {
                    pattern,
                    kind: Some(VarKind::Var),
                    body,
                    ..
                } => {
                    self.declare_pattern_names(pattern, env);
                    self.hoist_vars(std::slice::from_ref(body), env)?;
                }
                Stmt::ForEach { body, .. } => self.hoist_vars(std::slice::from_ref(body), env)?,
                Stmt::Try {
                    body,
                    catch,
                    finally,
                } => {
                    self.hoist_vars(std::slice::from_ref(body), env)?;
                    if let Some((_, body)) = catch {
                        self.hoist_vars(std::slice::from_ref(body), env)?;
                    }
                    if let Some(body) = finally {
                        self.hoist_vars(std::slice::from_ref(body), env)?;
                    }
                }
                Stmt::Switch(_, cases) => {
                    for (_, body) in cases {
                        self.hoist_vars(body, env)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn declare_pattern_names(&self, pattern: &Pattern, env: &Env) {
        match pattern {
            Pattern::Ident(name) => {
                env.borrow_mut()
                    .bindings
                    .entry(name.clone())
                    .or_insert(Value::Undefined);
            }
            Pattern::Array(items) => {
                for item in items.iter().flatten() {
                    self.declare_pattern_names(item, env);
                }
            }
            Pattern::Object(entries) => {
                for (_, item) in entries {
                    self.declare_pattern_names(item, env);
                }
            }
            Pattern::Rest(item) | Pattern::Default(item, _) => {
                self.declare_pattern_names(item, env)
            }
        }
    }

    fn bind_pattern(
        &mut self,
        doc: &mut BaseDocument,
        pattern: &Pattern,
        value: Value,
        env: &Env,
    ) -> Result<()> {
        match pattern {
            Pattern::Ident(name) => {
                env.borrow_mut().bindings.insert(name.clone(), value);
            }
            Pattern::Rest(inner) => self.bind_pattern(doc, inner, value, env)?,
            Pattern::Default(inner, fallback) => {
                let value = if matches!(value, Value::Undefined) {
                    self.eval_expr(doc, fallback, env)?
                } else {
                    value
                };
                self.bind_pattern(doc, inner, value, env)?;
            }
            Pattern::Array(items) => {
                let values = value.items();
                for (cursor, item) in items.iter().enumerate() {
                    if let Some(item) = item {
                        if let Pattern::Rest(inner) = item {
                            self.bind_pattern(
                                doc,
                                inner,
                                Value::array(values.iter().skip(cursor).cloned().collect()),
                                env,
                            )?;
                            continue;
                        }
                        self.bind_pattern(
                            doc,
                            item,
                            values.get(cursor).cloned().unwrap_or(Value::Undefined),
                            env,
                        )?;
                    }
                }
            }
            Pattern::Object(entries) => {
                let mut used = Vec::with_capacity(entries.len());
                for (key, item) in entries {
                    if let Pattern::Rest(inner) = item {
                        let rest = Value::object("Object");
                        for property in self.enumerable_keys(doc, &value)? {
                            let property = property_key(&property);
                            if !used.iter().any(|used_key| used_key == &property) {
                                rest.put(&property, self.get_property(doc, &value, &property)?);
                            }
                        }
                        self.bind_pattern(doc, inner, rest, env)?;
                        continue;
                    }
                    used.push(key.clone());
                    let field = self.get_property(doc, &value, key)?;
                    self.bind_pattern(doc, item, field, env)?;
                }
            }
        }
        Ok(())
    }

    fn bind_var_pattern(
        &mut self,
        doc: &mut BaseDocument,
        pattern: &Pattern,
        value: Value,
        env: &Env,
    ) -> Result<()> {
        match pattern {
            Pattern::Ident(name) => {
                if !Self::assign(env, name, value.clone()) {
                    self.declare(env, name, value);
                }
            }
            Pattern::Rest(inner) => self.bind_var_pattern(doc, inner, value, env)?,
            Pattern::Default(inner, fallback) => {
                let value = if matches!(value, Value::Undefined) {
                    self.eval_expr(doc, fallback, env)?
                } else {
                    value
                };
                self.bind_var_pattern(doc, inner, value, env)?;
            }
            Pattern::Array(items) => {
                let values = value.items();
                for (cursor, item) in items.iter().enumerate() {
                    if let Some(item) = item {
                        let value = if matches!(item, Pattern::Rest(_)) {
                            Value::array(values.iter().skip(cursor).cloned().collect())
                        } else {
                            values.get(cursor).cloned().unwrap_or(Value::Undefined)
                        };
                        self.bind_var_pattern(doc, item, value, env)?;
                    }
                }
            }
            Pattern::Object(entries) => {
                let mut used = Vec::with_capacity(entries.len());
                for (key, item) in entries {
                    if matches!(item, Pattern::Rest(_)) {
                        let rest = Value::object("Object");
                        for property in self.enumerable_keys(doc, &value)? {
                            let property = property_key(&property);
                            if !used.iter().any(|used_key| used_key == &property) {
                                rest.put(&property, self.get_property(doc, &value, &property)?);
                            }
                        }
                        self.bind_var_pattern(doc, item, rest, env)?;
                    } else {
                        used.push(key.clone());
                        let field = self.get_property(doc, &value, key)?;
                        self.bind_var_pattern(doc, item, field, env)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn make_function(&self, function: FunctionExpr, env: Env) -> Value {
        let name = function.name.clone();
        let source_offset = function.source_offset;
        let prototype = (!function.arrow).then(|| Value::object("Object"));
        let closure = Environment::new(Some(env));
        if let Some(prototype) = &prototype {
            closure
                .borrow_mut()
                .bindings
                .insert("\0function_prototype".into(), prototype.clone());
        }
        let lexical_this = if function.arrow {
            Self::lookup(&closure, "this").unwrap_or(Value::Undefined)
        } else {
            Value::Undefined
        };
        let function = Rc::new(Function {
            params: function.params,
            body: function.body,
            env: Rc::clone(&closure),
            arrow: function.arrow,
            asynchronous: function.asynchronous,
            source_offset,
            this: lexical_this,
            name,
        });
        let object = Value::object("Function");
        if let Some(object) = object.as_object() {
            let mut object = object.borrow_mut();
            object.function = Some(Rc::clone(&function));
            object.props.insert(
                "name".into(),
                Property::data(Value::string(function.name.as_deref().unwrap_or(""))),
            );
            if let Some(prototype) = prototype {
                object
                    .props
                    .insert("prototype".into(), Property::data(prototype));
            }
        }
        let self_ref = Value::object("FunctionSelfRef");
        if let (Some(reference), Some(function_object)) = (self_ref.as_object(), object.as_object())
        {
            let weak: Weak<std::cell::RefCell<Object>> = Rc::downgrade(&function_object);
            reference.borrow_mut().payload = Some(Rc::new(weak));
        }
        if let Some(name) = &function.name {
            if !name.is_empty() {
                closure
                    .borrow_mut()
                    .bindings
                    .insert(name.clone(), self_ref.clone());
            }
        }
        closure
            .borrow_mut()
            .bindings
            .insert("\0self".into(), self_ref);
        object
    }

    fn exec_statements(
        &mut self,
        doc: &mut BaseDocument,
        statements: &[Stmt],
        env: &Env,
    ) -> ExecResult {
        for statement in statements {
            self.tick()?;
            match self.exec_stmt(doc, statement, env)? {
                Flow::Normal => {}
                Flow::Return(value) => return Ok(Flow::Return(value)),
                Flow::Break => return Ok(Flow::Break),
                Flow::Continue => return Ok(Flow::Continue),
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_stmt(&mut self, doc: &mut BaseDocument, stmt: &Stmt, env: &Env) -> ExecResult {
        match stmt {
            Stmt::Empty => Ok(Flow::Normal),
            Stmt::Expr(expr) => {
                self.eval_expr(doc, expr, env)?;
                Ok(Flow::Normal)
            }
            Stmt::Block(body) => {
                let block = Environment::new(Some(Rc::clone(env)));
                self.hoist(doc, body, &block)?;
                self.exec_statements(doc, body, &block)
            }
            Stmt::Var(kind, declarations) => {
                for (pattern, init) in declarations {
                    if matches!(kind, VarKind::Var) && init.is_none() {
                        continue;
                    }
                    let value = if let Some(init) = init {
                        self.eval_expr(doc, init, env)?
                    } else {
                        Value::Undefined
                    };
                    if matches!(kind, VarKind::Var) {
                        self.bind_var_pattern(doc, pattern, value, env)?;
                    } else {
                        self.bind_pattern(doc, pattern, value, env)?;
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::FunctionDecl(_, _) | Stmt::ClassDecl(_, _) => Ok(Flow::Normal),
            Stmt::Return(expr) => Ok(Flow::Return(match expr {
                Some(expr) => self.eval_expr(doc, expr, env)?,
                None => Value::Undefined,
            })),
            Stmt::Throw(expr) => Err(Error::thrown(self.eval_expr(doc, expr, env)?)),
            Stmt::If(test, yes, no) => {
                if self.eval_expr(doc, test, env)?.truthy() {
                    self.exec_stmt(doc, yes, env)
                } else if let Some(no) = no {
                    self.exec_stmt(doc, no, env)
                } else {
                    Ok(Flow::Normal)
                }
            }
            Stmt::While(test, body) => {
                while self.eval_expr(doc, test, env)?.truthy() {
                    self.tick()?;
                    match self.exec_stmt(doc, body, env)? {
                        Flow::Normal | Flow::Continue => {}
                        Flow::Break => break,
                        other => return Ok(other),
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::DoWhile(body, test) => {
                loop {
                    self.tick()?;
                    match self.exec_stmt(doc, body, env)? {
                        Flow::Normal | Flow::Continue => {}
                        Flow::Break => break,
                        other => return Ok(other),
                    }
                    if !self.eval_expr(doc, test, env)?.truthy() {
                        break;
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::For {
                init,
                test,
                update,
                body,
            } => {
                let lexical_loop = !matches!(init.as_deref(), Some(Stmt::Var(VarKind::Var, _)));
                let loop_env = if lexical_loop {
                    Environment::new(Some(Rc::clone(env)))
                } else {
                    Rc::clone(env)
                };
                if let Some(init) = init {
                    let _ = self.exec_stmt(doc, init, &loop_env)?;
                }
                loop {
                    if let Some(test) = test {
                        if !self.eval_expr(doc, test, &loop_env)?.truthy() {
                            break;
                        }
                    }
                    self.tick()?;
                    match self.exec_stmt(doc, body, &loop_env)? {
                        Flow::Normal | Flow::Continue => {}
                        Flow::Break => break,
                        other => return Ok(other),
                    }
                    if let Some(update) = update {
                        self.eval_expr(doc, update, &loop_env)?;
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::ForEach {
                pattern,
                kind,
                iterable,
                body,
                keys,
            } => {
                let source = self.eval_expr(doc, iterable, env)?;
                let values = if *keys {
                    self.enumerable_string_keys(doc, &source)?
                } else {
                    self.iterable_values(doc, &source)?
                };
                for value in values.into_iter().take(MAX_ARRAY_LENGTH) {
                    self.tick()?;
                    let loop_env = if matches!(kind, Some(VarKind::Let | VarKind::Const)) {
                        Environment::new(Some(Rc::clone(env)))
                    } else {
                        Rc::clone(env)
                    };
                    if !matches!(kind, Some(VarKind::Let | VarKind::Const)) {
                        self.bind_var_pattern(doc, pattern, value, &loop_env)?;
                    } else {
                        self.bind_pattern(doc, pattern, value, &loop_env)?;
                    }
                    match self.exec_stmt(doc, body, &loop_env)? {
                        Flow::Normal | Flow::Continue => {}
                        Flow::Break => break,
                        other => return Ok(other),
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Try {
                body,
                catch,
                finally,
            } => {
                let attempted = self.exec_stmt(doc, body, env);
                let caught = match attempted {
                    Err(error) => {
                        if let Some((pattern, handler)) = catch {
                            let catch_env = Environment::new(Some(Rc::clone(env)));
                            if let Some(pattern) = pattern {
                                let thrown = self.error_value(&error);
                                self.bind_pattern(doc, pattern, thrown, &catch_env)?;
                            }
                            self.exec_stmt(doc, handler, &catch_env)
                        } else {
                            Err(error)
                        }
                    }
                    flow => flow,
                };
                if let Some(finally) = finally {
                    match self.exec_stmt(doc, finally, env)? {
                        Flow::Normal => {}
                        override_flow => return Ok(override_flow),
                    }
                }
                caught
            }
            Stmt::Switch(value, cases) => {
                let switch_env = Environment::new(Some(Rc::clone(env)));
                let switch_statements = cases
                    .iter()
                    .flat_map(|(_, body)| body.iter().cloned())
                    .collect::<Vec<_>>();
                self.hoist(doc, &switch_statements, &switch_env)?;
                let discriminant = self.eval_expr(doc, value, &switch_env)?;
                let mut start = None;
                let mut default = None;
                for (index, (test, _)) in cases.iter().enumerate() {
                    if let Some(test) = test {
                        let value = self.eval_expr(doc, test, &switch_env)?;
                        if self.strict_equal(&discriminant, &value) {
                            start = Some(index);
                            break;
                        }
                    } else {
                        default = Some(index);
                    }
                }
                if start.is_none() {
                    start = default;
                }
                if let Some(start) = start {
                    for (_, body) in cases.iter().skip(start) {
                        match self.exec_statements(doc, body, &switch_env)? {
                            Flow::Break => break,
                            other @ Flow::Return(_) => return Ok(other),
                            Flow::Continue => return Ok(Flow::Continue),
                            Flow::Normal => {}
                        }
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Break => Ok(Flow::Break),
            Stmt::Continue => Ok(Flow::Continue),
        }
    }

    fn eval_expr(&mut self, doc: &mut BaseDocument, expr: &Expr, env: &Env) -> Result<Value> {
        self.tick()?;
        match expr {
            Expr::Literal(literal) => Ok(match literal {
                Literal::Undefined => Value::Undefined,
                Literal::Null => Value::Null,
                Literal::Bool(value) => Value::Bool(*value),
                Literal::Number(value) => Value::Number(*value),
                Literal::String(value) => Value::String(Rc::from(value.clone())),
            }),
            Expr::Ident(name) => {
                if let Some(value) =
                    Self::lookup(env, name).or_else(|| Self::lookup(&self.env, name))
                {
                    Ok(value)
                } else {
                    let value = self.get_property(doc, &self.global.clone(), name)?;
                    if matches!(value, Value::Undefined) {
                        Err(Error::new(
                            "ReferenceError",
                            format!("{name} is not defined"),
                        ))
                    } else {
                        Ok(value)
                    }
                }
            }
            Expr::This => Ok(Self::lookup(env, "this").unwrap_or(Value::Undefined)),
            Expr::Array(items) => {
                let mut values = Vec::with_capacity(items.len().min(MAX_ARRAY_LENGTH));
                for item in items {
                    match item {
                        Some(Expr::Spread(source)) => {
                            let source = self.eval_expr(doc, source, env)?;
                            let spread = self.iterable_values(doc, &source)?;
                            if values.len().saturating_add(spread.len()) > MAX_ARRAY_LENGTH {
                                return Err(Error::new(
                                    "RangeError",
                                    "array literal exceeds the element limit",
                                ));
                            }
                            values.extend(spread);
                        }
                        Some(expr) => values.push(self.eval_expr(doc, expr, env)?),
                        None => values.push(Value::Undefined),
                    }
                    if values.len() > MAX_ARRAY_LENGTH {
                        return Err(Error::new(
                            "RangeError",
                            "array literal exceeds the element limit",
                        ));
                    }
                }
                Ok(Value::array(values))
            }
            Expr::Object(entries) => {
                let object = Value::object("Object");
                for entry in entries {
                    match entry {
                        ObjectEntry::Property(key, value) => {
                            let value = self.eval_expr(doc, value, env)?;
                            object.put(key, value);
                        }
                        ObjectEntry::Computed(key, value) => {
                            let key = property_key(&self.eval_expr(doc, key, env)?);
                            let value = self.eval_expr(doc, value, env)?;
                            self.set_property(doc, &object, &key, value)?;
                        }
                        ObjectEntry::Spread(value) => {
                            let spread = self.eval_expr(doc, value, env)?;
                            for key in self.enumerable_keys(doc, &spread)? {
                                let key = property_key(&key);
                                let value = self.get_property(doc, &spread, &key)?;
                                self.set_property(doc, &object, &key, value)?;
                            }
                        }
                        ObjectEntry::Method(key, function) => {
                            object.put(key, self.make_function(function.clone(), Rc::clone(env)));
                        }
                        ObjectEntry::Getter(key, function) => {
                            let getter = self.make_function(function.clone(), Rc::clone(env));
                            if let Some(obj) = object.as_object() {
                                let mut obj = obj.borrow_mut();
                                let prop = obj
                                    .props
                                    .entry(key.clone())
                                    .or_insert_with(|| Property::data(Value::Undefined));
                                prop.get = Some(getter);
                            }
                        }
                        ObjectEntry::Setter(key, function) => {
                            let setter = self.make_function(function.clone(), Rc::clone(env));
                            if let Some(obj) = object.as_object() {
                                let mut obj = obj.borrow_mut();
                                let prop = obj
                                    .props
                                    .entry(key.clone())
                                    .or_insert_with(|| Property::data(Value::Undefined));
                                prop.set = Some(setter);
                            }
                        }
                    }
                }
                Ok(object)
            }
            Expr::Function(function) => Ok(self.make_function(function.clone(), Rc::clone(env))),
            Expr::Class(class) => self.eval_class(doc, class, env),
            Expr::Unary(op, value) => {
                if op == "delete" {
                    return self.delete_target(doc, Some(value), env).map(Value::Bool);
                }
                let value = match self.eval_expr(doc, value, env) {
                    Err(error)
                        if op == "typeof"
                            && matches!(value.as_ref(), Expr::Ident(_))
                            && error.name == "ReferenceError" =>
                    {
                        Value::Undefined
                    }
                    result => result?,
                };
                match op.as_str() {
                    "!" => Ok(Value::Bool(!value.truthy())),
                    "void" => Ok(Value::Undefined),
                    "typeof" => Ok(Value::string(match &value {
                        Value::Undefined => "undefined",
                        Value::Null => "object",
                        Value::Bool(_) => "boolean",
                        Value::Number(_) => "number",
                        Value::String(_) => "string",
                        Value::Object(_) if value.kind() == "Symbol" => "symbol",
                        Value::Object(_) if super::proxy::is_callable(&value) => "function",
                        _ => "object",
                    })),
                    "+" => Ok(Value::Number(to_number(&value))),
                    "-" => Ok(Value::Number(-to_number(&value))),
                    "~" => Ok(Value::Number((!(to_number(&value) as i32)) as f64)),
                    _ => Err(Error::syntax(format!("unsupported unary operator {op}"))),
                }
            }
            Expr::Binary(op, left, right) => {
                let left = self.eval_expr(doc, left, env)?;
                if op == "&&" && !left.truthy() {
                    return Ok(left);
                }
                if op == "||" && left.truthy() {
                    return Ok(left);
                }
                if op == "??" && !matches!(left, Value::Null | Value::Undefined) {
                    return Ok(left);
                }
                let right = self.eval_expr(doc, right, env)?;
                self.binary(doc, op, left, right)
            }
            Expr::Conditional(test, yes, no) => {
                if self.eval_expr(doc, test, env)?.truthy() {
                    self.eval_expr(doc, yes, env)
                } else {
                    self.eval_expr(doc, no, env)
                }
            }
            Expr::Assign(op, target, rhs) => {
                if op == "=" {
                    let value = self.eval_expr(doc, rhs, env)?;
                    self.assign_target(doc, target, env, value.clone())?;
                    return Ok(value);
                }
                let current = self.eval_expr(doc, target, env)?;
                if op == "&&=" && !current.truthy()
                    || op == "||=" && current.truthy()
                    || op == "??=" && !matches!(current, Value::Null | Value::Undefined)
                {
                    return Ok(current);
                }
                let value = self.eval_expr(doc, rhs, env)?;
                let result = self.binary(doc, op.trim_end_matches('='), current, value)?;
                self.assign_target(doc, target, env, result.clone())?;
                Ok(result)
            }
            Expr::Update(op, target, prefix) => {
                let old = self.eval_expr(doc, target, env)?;
                let delta = if op == "++" {
                    1.0
                } else if op == "--" {
                    -1.0
                } else {
                    return Err(Error::syntax("invalid update operator"));
                };
                let next = Value::Number(to_number(&old) + delta);
                self.assign_target(doc, target, env, next.clone())?;
                Ok(if *prefix { next } else { old })
            }
            Expr::Member(target, key) => {
                let object = self.eval_expr(doc, target, env)?;
                let key = property_key(&self.eval_expr(doc, key, env)?);
                let mut label = call_label(target);
                append_call_label(&mut label, ".");
                append_call_label(&mut label, &key);
                self.get_property(doc, &object, &key).map_err(|mut error| {
                    error.stack.insert(0, format!("read {label}"));
                    error
                })
            }
            Expr::Call(callee, arguments) => {
                if matches!(callee.as_ref(),Expr::Ident(name) if name == "super") {
                    let parent = Self::lookup(env, "super")
                        .ok_or_else(|| Error::syntax("super has no parent constructor"))?;
                    if !env
                        .borrow()
                        .bindings
                        .get("\0derived_constructor_call")
                        .is_some_and(Value::truthy)
                    {
                        return Err(Error::syntax(
                            "super() is only valid in a derived constructor",
                        ));
                    }
                    if env.borrow().bindings.contains_key("\0super_called") {
                        return Err(Error::type_error(
                            "super constructor may only be called once",
                        ));
                    }
                    let args = self.eval_arguments(doc, arguments, env)?;
                    let instance = self.call(doc, parent, Value::Undefined, args, true)?;
                    if let Some(object) = instance.as_object() {
                        if let Some(prototype) = Self::lookup(env, "\0function_prototype") {
                            object.borrow_mut().prototype = Some(prototype);
                        }
                    } else {
                        return Err(Error::type_error(
                            "super constructor did not return an object",
                        ));
                    }
                    Self::assign(env, "this", instance.clone());
                    env.borrow_mut()
                        .bindings
                        .insert("\0super_called".into(), Value::Bool(true));
                    let fields = Self::lookup(env, "\0instance_fields")
                        .map(|metadata| class_fields(&metadata))
                        .unwrap_or_default();
                    let field_parent = env
                        .borrow()
                        .parent
                        .clone()
                        .unwrap_or_else(|| Rc::clone(env));
                    let field_env = Environment::new(Some(field_parent));
                    field_env
                        .borrow_mut()
                        .bindings
                        .insert("this".into(), instance.clone());
                    for (name, initial) in fields {
                        let value = if let Some(initial) = initial {
                            self.eval_expr(doc, &initial, &field_env)?
                        } else {
                            Value::Undefined
                        };
                        self.set_property(doc, &instance, &name, value)?;
                    }
                    return Ok(instance);
                }
                let (function, receiver, label) = match callee.as_ref() {
                    Expr::Member(object, key) => {
                        let receiver = self.eval_expr(doc, object, env)?;
                        let key = property_key(&self.eval_expr(doc, key, env)?);
                        let function = self.get_property(doc, &receiver, &key)?;
                        let mut label = call_label(object);
                        append_call_label(&mut label, ".");
                        append_call_label(&mut label, &key);
                        (function, receiver, label)
                    }
                    _ => (
                        self.eval_expr(doc, callee, env)?,
                        Value::Undefined,
                        call_label(callee),
                    ),
                };
                let args = self.eval_arguments(doc, arguments, env)?;
                self.call(doc, function, receiver, args, false)
                    .map_err(|mut error| {
                        error.stack.insert(0, format!("call {label}"));
                        error
                    })
            }
            Expr::New(callee, arguments) => {
                let constructor = self.eval_expr(doc, callee, env)?;
                let args = self.eval_arguments(doc, arguments, env)?;
                self.call(doc, constructor, Value::Undefined, args, true)
            }
            Expr::Sequence(expressions) => {
                let mut value = Value::Undefined;
                for expression in expressions {
                    value = self.eval_expr(doc, expression, env)?;
                }
                Ok(value)
            }
            Expr::Await(value) => {
                let value = self.eval_expr(doc, value, env)?;
                self.await_value(doc, value)
            }
            Expr::Spread(value) => self.eval_expr(doc, value, env),
            Expr::Chain(base, parts) => {
                let mut value = self.eval_expr(doc, base, env)?;
                let mut receiver = Value::Undefined;
                let mut label = call_label(base);
                for part in parts {
                    self.tick()?;
                    match part {
                        ChainPart::Member(key, optional) => {
                            if *optional && matches!(value, Value::Null | Value::Undefined) {
                                return Ok(Value::Undefined);
                            }
                            let key = property_key(&self.eval_expr(doc, key, env)?);
                            receiver = value;
                            let mut read_label = label.clone();
                            append_call_label(&mut read_label, ".");
                            append_call_label(&mut read_label, &key);
                            value =
                                self.get_property(doc, &receiver, &key)
                                    .map_err(|mut error| {
                                        error.stack.insert(0, format!("read {read_label}"));
                                        error
                                    })?;
                            append_call_label(&mut label, ".");
                            append_call_label(&mut label, &key);
                        }
                        ChainPart::Call(arguments, optional) => {
                            if *optional && matches!(value, Value::Null | Value::Undefined) {
                                return Ok(Value::Undefined);
                            }
                            let arguments = self.eval_arguments(doc, arguments, env)?;
                            value = self.call(doc, value, receiver, arguments, false).map_err(
                                |mut error| {
                                    error.stack.insert(0, format!("call {label}"));
                                    error
                                },
                            )?;
                            append_call_label(&mut label, "()");
                            receiver = Value::Undefined;
                        }
                    }
                }
                Ok(value)
            }
            Expr::Template(parts) => {
                let mut text = String::new();
                for part in parts {
                    match part {
                        TemplatePart::Text(units) => {
                            text.push_str(&String::from_utf16_lossy(units))
                        }
                        TemplatePart::Expression(expr) => {
                            text.push_str(&self.eval_expr(doc, expr, env)?.text())
                        }
                    }
                }
                Ok(Value::string(text))
            }
        }
    }

    fn eval_arguments(
        &mut self,
        doc: &mut BaseDocument,
        args: &[Expr],
        env: &Env,
    ) -> Result<Vec<Value>> {
        let mut values = Vec::new();
        for arg in args {
            if let Expr::Spread(expr) = arg {
                let value = self.eval_expr(doc, expr, env)?;
                values.extend(self.iterable_values(doc, &value)?);
            } else {
                values.push(self.eval_expr(doc, arg, env)?);
            }
            if values.len() > MAX_ARRAY_LENGTH {
                return Err(Error::new("RangeError", "argument list exceeds limit"));
            }
        }
        Ok(values)
    }

    fn eval_class(
        &mut self,
        doc: &mut BaseDocument,
        class: &ClassExpr,
        env: &Env,
    ) -> Result<Value> {
        let env = Environment::new(Some(Rc::clone(env)));
        let parent_constructor = if let Some(parent) = &class.extends {
            let parent = self.eval_expr(doc, parent, &env)?;
            env.borrow_mut().bindings.insert("super".into(), parent);
            Some(Self::lookup(&env, "super").unwrap_or(Value::Undefined))
        } else {
            None
        };
        let derived = parent_constructor.is_some();
        let constructor = class
            .methods
            .iter()
            .find(|method| method.name == "constructor" && !method.is_static);
        let has_explicit_constructor = constructor.is_some();
        let mut constructor =
            constructor
                .map(|method| method.function.clone())
                .unwrap_or(FunctionExpr {
                    params: Vec::new(),
                    body: Vec::new(),
                    arrow: false,
                    asynchronous: false,
                    name: class.name.clone(),
                    source_offset: None,
                });
        let fields = class
            .fields
            .iter()
            .filter(|(_, _, is_static)| !*is_static)
            .map(|(name, initial, _)| (name.clone(), initial.clone()))
            .collect::<Vec<_>>();
        if derived {
            if !has_explicit_constructor {
                constructor.body.push(Stmt::Expr(Expr::Call(
                    Box::new(Expr::Ident("super".into())),
                    vec![Expr::Spread(Box::new(Expr::Ident("arguments".into())))],
                )));
            }
        }
        let function = self.make_function(constructor, Rc::clone(&env));
        let field_metadata = Value::object("ClassFieldMetadata");
        if let Some(object) = field_metadata.as_object() {
            object.borrow_mut().payload = Some(Rc::new(fields));
        }
        if let Some(function_data) = function
            .as_object()
            .and_then(|object| object.borrow().function.clone())
        {
            function_data
                .env
                .borrow_mut()
                .bindings
                .insert("\0instance_fields".into(), field_metadata);
            if derived {
                function_data
                    .env
                    .borrow_mut()
                    .bindings
                    .insert("\0derived_constructor".into(), Value::Bool(true));
            }
        }
        if let Some(name) = &class.name {
            env.borrow_mut()
                .bindings
                .insert(name.clone(), function.clone());
        }
        let prototype = Value::object("Object");
        if let Some(parent) = &parent_constructor {
            let parent_proto = self.get_property(doc, parent, "prototype")?;
            if let Some(proto) = prototype.as_object() {
                proto.borrow_mut().prototype = Some(parent_proto);
            }
            if let Some(function_object) = function.as_object() {
                function_object.borrow_mut().prototype = Some(parent.clone());
            }
        }
        for method in &class.methods {
            if method.name == "constructor" {
                continue;
            }
            let value = self.make_function(method.function.clone(), Rc::clone(&env));
            let target = if method.is_static {
                &function
            } else {
                &prototype
            };
            if method.getter || method.setter {
                if let Some(object) = target.as_object() {
                    let mut object = object.borrow_mut();
                    let property = object
                        .props
                        .entry(method.name.clone())
                        .or_insert_with(|| Property::data(Value::Undefined));
                    if method.getter {
                        property.get = Some(value);
                    } else {
                        property.set = Some(value);
                    }
                }
            } else {
                target.put(&method.name, value);
            }
        }
        function.put("prototype", prototype.clone());
        if let Some(function_data) = function
            .as_object()
            .and_then(|object| object.borrow().function.clone())
        {
            function_data
                .env
                .borrow_mut()
                .bindings
                .insert("\0function_prototype".into(), prototype);
        }
        for (name, initial, is_static) in &class.fields {
            if *is_static {
                let value = if let Some(initial) = initial {
                    self.eval_expr(doc, initial, &env)?
                } else {
                    Value::Undefined
                };
                function.put(name, value);
            }
        }
        if let Some(name) = &class.name {
            function.put("name", Value::string(name));
        }
        Ok(function)
    }

    fn binary(
        &mut self,
        doc: &mut BaseDocument,
        op: &str,
        left: Value,
        right: Value,
    ) -> Result<Value> {
        let ln = to_number(&left);
        let rn = to_number(&right);
        let order = match (&left, &right) {
            (Value::String(left), Value::String(right)) => Some(left.cmp(right)),
            _ => ln.partial_cmp(&rn),
        };
        Ok(match op {
            "+" => {
                if matches!(left, Value::String(_)) || matches!(right, Value::String(_)) {
                    Value::string(format!("{}{}", left.text(), right.text()))
                } else {
                    Value::Number(ln + rn)
                }
            }
            "-" => Value::Number(ln - rn),
            "*" => Value::Number(ln * rn),
            "/" => Value::Number(ln / rn),
            "%" => Value::Number(ln % rn),
            "**" => Value::Number(ln.powf(rn)),
            "===" => Value::Bool(self.strict_equal(&left, &right)),
            "!==" => Value::Bool(!self.strict_equal(&left, &right)),
            "==" => Value::Bool(self.loose_equal(&left, &right)),
            "!=" => Value::Bool(!self.loose_equal(&left, &right)),
            "<" => Value::Bool(order == Some(std::cmp::Ordering::Less)),
            ">" => Value::Bool(order == Some(std::cmp::Ordering::Greater)),
            "<=" => Value::Bool(matches!(
                order,
                Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
            )),
            ">=" => Value::Bool(matches!(
                order,
                Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
            )),
            "&&" | "||" | "??" => right,
            "|" => Value::Number(((ln as i32) | (rn as i32)) as f64),
            "&" => Value::Number(((ln as i32) & (rn as i32)) as f64),
            "^" => Value::Number(((ln as i32) ^ (rn as i32)) as f64),
            "<<" => Value::Number(((ln as i32) << ((rn as u32) & 31)) as f64),
            ">>" => Value::Number(((ln as i32) >> ((rn as u32) & 31)) as f64),
            ">>>" => Value::Number(((ln as u32) >> ((rn as u32) & 31)) as f64),
            "in" => {
                if !matches!(&right, Value::Object(_)) {
                    return Err(Error::type_error("right operand of in is not an object"));
                }
                Value::Bool(self.has_property(doc, &right, &property_key(&left))?)
            }
            "instanceof" => Value::Bool(self.instance_of(doc, &left, &right)?),
            _ => return Err(Error::syntax(format!("unsupported binary operator {op}"))),
        })
    }

    fn strict_equal(&self, left: &Value, right: &Value) -> bool {
        match (left, right) {
            (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Object(_), Value::Object(_)) => left.same(right),
            _ => false,
        }
    }

    fn loose_equal(&self, left: &Value, right: &Value) -> bool {
        if self.strict_equal(left, right) {
            return true;
        }
        match (left, right) {
            (Value::Null, Value::Undefined) | (Value::Undefined, Value::Null) => true,
            (Value::Number(_), Value::String(_)) | (Value::String(_), Value::Number(_)) => {
                to_number(left) == to_number(right)
            }
            (Value::Bool(_), _) => to_number(left) == to_number(right),
            (_, Value::Bool(_)) => to_number(left) == to_number(right),
            (Value::Object(_), Value::String(_) | Value::Number(_)) => {
                self.loose_equal(&Value::string(left.text()), right)
            }
            (Value::String(_) | Value::Number(_), Value::Object(_)) => {
                self.loose_equal(left, &Value::string(right.text()))
            }
            _ => false,
        }
    }

    fn assign_target(
        &mut self,
        doc: &mut BaseDocument,
        target: &Expr,
        env: &Env,
        value: Value,
    ) -> Result<()> {
        match target {
            Expr::Ident(name) => {
                let global_binding = self.env.borrow().bindings.contains_key(name);
                let unbound = !Self::assign(env, name, value.clone())
                    && !Self::assign(&self.env, name, value.clone());
                if unbound || global_binding {
                    self.global.put(name, value);
                }
                Ok(())
            }
            Expr::Member(object, key) => {
                let object = self.eval_expr(doc, object, env)?;
                let key = property_key(&self.eval_expr(doc, key, env)?);
                self.set_property(doc, &object, &key, value)
            }
            _ => Err(Error::syntax("invalid assignment target")),
        }
    }

    fn delete_target(
        &mut self,
        doc: &mut BaseDocument,
        target: Option<&Expr>,
        env: &Env,
    ) -> Result<bool> {
        let Some(target) = target else {
            return Ok(false);
        };
        if let Expr::Member(object, key) = target {
            let object = self.eval_expr(doc, object, env)?;
            let key = property_key(&self.eval_expr(doc, key, env)?);
            if object.kind() == "Proxy" {
                return Ok(self.proxy_delete(doc, &object, &key)?.unwrap_or(false));
            }
            if let Some(object) = object.as_object() {
                let mut object = object.borrow_mut();
                if object.props.remove(&key).is_some() {
                    return Ok(true);
                }
                if let Ok(index) = key.parse::<usize>() {
                    if let Some(value) = object.indexed.get_mut(index) {
                        *value = Value::Undefined;
                        return Ok(true);
                    }
                }
                return Ok(false);
            }
        }
        Ok(false)
    }

    fn enumerable_keys(&mut self, doc: &mut BaseDocument, value: &Value) -> Result<Vec<Value>> {
        if value.as_object().is_some() {
            let mut keys = Vec::new();
            for key in self.own_property_names(doc, value)? {
                if self
                    .ordinary_descriptor(doc, value, &key)?
                    .get("enumerable")
                    .truthy()
                {
                    keys.push(property_key_value(&self.global, &key));
                }
            }
            return Ok(keys);
        }
        if let Value::String(units) = value {
            return Ok((0..units.len())
                .map(|index| Value::string(index.to_string()))
                .collect());
        }
        let _ = doc;
        Ok(Vec::new())
    }

    fn enumerable_string_keys(
        &mut self,
        doc: &mut BaseDocument,
        value: &Value,
    ) -> Result<Vec<Value>> {
        Ok(self
            .enumerable_keys(doc, value)?
            .into_iter()
            .filter(|key| key.kind() != "Symbol")
            .collect())
    }

    pub(super) fn iterable_values(
        &mut self,
        doc: &mut BaseDocument,
        value: &Value,
    ) -> Result<Vec<Value>> {
        if matches!(value, Value::Object(_)) {
            let iterator = self.get_property(doc, value, "@@iterator")?;
            if !matches!(iterator, Value::Undefined) {
                let iterator = self.call(doc, iterator, value.clone(), Vec::new(), false)?;
                let mut out = Vec::new();
                for _ in 0..MAX_ARRAY_LENGTH {
                    let next = self.get_property(doc, &iterator, "next")?;
                    let step = self.call(doc, next, iterator.clone(), Vec::new(), false)?;
                    if self.get_property(doc, &step, "done")?.truthy() {
                        return Ok(out);
                    }
                    out.push(self.get_property(doc, &step, "value")?);
                }
                return Err(Error::new("RangeError", "iterator exceeded element limit"));
            }
        }
        let items = value.items();
        if !items.is_empty() || value.kind() == "Array" || value.kind() == "NodeList" {
            return Ok(items.into_iter().take(MAX_ARRAY_LENGTH).collect());
        }
        if let Value::String(units) = value {
            return Ok(String::from_utf16_lossy(units)
                .chars()
                .map(|c| Value::string(c.to_string()))
                .collect());
        }
        Err(Error::type_error("value is not iterable"))
    }

    fn drain_one_job(&mut self, doc: &mut BaseDocument) -> Result<bool> {
        if let Some(job) = self.jobs.pop_front() {
            let callable = job.callback.clone();
            self.call(doc, callable, job.this, job.args, false)?;
            self.tick()?;
            return Ok(true);
        }
        let host_jobs = self.bridge.poll(doc, None, true)?;
        self.jobs.extend(host_jobs);
        if let Some(job) = self.jobs.pop_front() {
            self.call(doc, job.callback, job.this, job.args, false)?;
            self.tick()?;
            return Ok(true);
        }
        Ok(false)
    }
}

fn to_number(value: &Value) -> f64 {
    match value {
        Value::Undefined => f64::NAN,
        Value::Null => 0.0,
        Value::Bool(value) => {
            if *value {
                1.0
            } else {
                0.0
            }
        }
        Value::Number(value) => *value,
        Value::String(value) => String::from_utf16_lossy(value)
            .trim()
            .parse()
            .unwrap_or_else(|_| {
                if String::from_utf16_lossy(value).trim().is_empty() {
                    0.0
                } else {
                    f64::NAN
                }
            }),
        Value::Object(_) => value.text().parse().unwrap_or(f64::NAN),
    }
}

pub(super) fn property_key(value: &Value) -> String {
    if value.kind() == "Symbol" {
        if let Some(name) = value.as_object().and_then(|object| {
            object
                .borrow()
                .payload
                .as_ref()
                .and_then(|payload| payload.downcast_ref::<String>().cloned())
        }) {
            if name == "Symbol.iterator" {
                return "@@iterator".into();
            }
            return format!("@@{name}");
        }
    }
    value.text()
}

pub(super) fn property_key_value(global: &Value, key: &str) -> Value {
    let symbol = global.get("\0symbol_keys").get(key);
    if symbol.kind() == "Symbol" {
        symbol
    } else {
        Value::string(key)
    }
}

pub(super) fn is_symbol_property_key(global: &Value, key: &str) -> bool {
    global.get("\0symbol_keys").get(key).kind() == "Symbol"
}
