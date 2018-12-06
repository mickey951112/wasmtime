use cranelift_codegen::isa;
use spectest::SpecTest;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::io::Read;
use std::path::Path;
use std::str;
use wabt::script::{self, Action, Command, CommandKind, ModuleBinary, ScriptParser};
use wasmtime_execute::{ActionOutcome, Code, InstanceWorld, Value};

struct Instances {
    current: Option<InstanceWorld>,
    namespace: HashMap<String, InstanceWorld>,
    code: Code,
    spectest: SpecTest,
}

impl Instances {
    pub fn new() -> Self {
        Self {
            current: None,
            namespace: HashMap::new(),
            code: Code::new(),
            spectest: SpecTest::new(),
        }
    }

    fn instantiate(
        &mut self,
        isa: &isa::TargetIsa,
        module: ModuleBinary,
    ) -> Result<InstanceWorld, String> {
        InstanceWorld::new(&mut self.code, isa, &module.into_vec(), &mut self.spectest)
    }

    pub fn define_unnamed_module(
        &mut self,
        isa: &isa::TargetIsa,
        module: ModuleBinary,
    ) -> Result<(), String> {
        self.current = Some(self.instantiate(isa, module)?);
        Ok(())
    }

    pub fn define_named_module(
        &mut self,
        isa: &isa::TargetIsa,
        name: String,
        module: ModuleBinary,
    ) -> Result<(), String> {
        let world = self.instantiate(isa, module)?;
        self.namespace.insert(name, world);
        Ok(())
    }

    pub fn perform_action(
        &mut self,
        isa: &isa::TargetIsa,
        action: Action,
    ) -> Result<ActionOutcome, String> {
        match action {
            Action::Invoke {
                module,
                field,
                args,
            } => {
                let mut value_args = Vec::with_capacity(args.len());
                for a in args {
                    value_args.push(match a {
                        script::Value::I32(i) => Value::I32(i),
                        script::Value::I64(i) => Value::I64(i),
                        script::Value::F32(i) => Value::F32(i.to_bits()),
                        script::Value::F64(i) => Value::F64(i.to_bits()),
                    });
                }
                match module {
                    None => match self.current {
                        None => Err("invoke performed with no module present".to_string()),
                        Some(ref mut instance_world) => instance_world
                            .invoke(&mut self.code, isa, &field, &value_args)
                            .map_err(|e| {
                                format!("error invoking {} in current module: {}", field, e)
                            }),
                    },
                    Some(name) => self
                        .namespace
                        .get_mut(&name)
                        .ok_or_else(|| format!("module {} not declared", name))?
                        .invoke(&mut self.code, isa, &field, &value_args)
                        .map_err(|e| format!("error invoking {} in module {}: {}", field, name, e)),
                }
            }
            Action::Get { module, field } => {
                let value = match module {
                    None => match self.current {
                        None => return Err("get performed with no module present".to_string()),
                        Some(ref mut instance_world) => {
                            instance_world.get(&field).map_err(|e| {
                                format!("error getting {} in current module: {}", field, e)
                            })?
                        }
                    },
                    Some(name) => self
                        .namespace
                        .get_mut(&name)
                        .ok_or_else(|| format!("module {} not declared", name))?
                        .get(&field)
                        .map_err(|e| {
                            format!("error getting {} in module {}: {}", field, name, e)
                        })?,
                };
                Ok(ActionOutcome::Returned {
                    values: vec![value],
                })
            }
        }
    }
}

/// Run a wast script from a byte buffer.
pub fn wast_buffer(name: &str, isa: &isa::TargetIsa, wast: &[u8]) -> Result<(), String> {
    let mut parser = ScriptParser::from_str(str::from_utf8(wast).unwrap()).unwrap();
    let mut instances = Instances::new();

    while let Some(Command { kind, line }) = parser.next().unwrap() {
        match kind {
            CommandKind::Module { module, name } => {
                if let Some(name) = name {
                    instances.define_named_module(isa, name, module.clone())?;
                }

                instances.define_unnamed_module(isa, module)?;
            }
            CommandKind::Register {
                name: _name,
                as_name: _as_name,
            } => {
                println!("{}:{}: TODO: Implement register", name, line);
            }
            CommandKind::PerformAction(action) => match instances.perform_action(isa, action)? {
                ActionOutcome::Returned { .. } => {}
                ActionOutcome::Trapped { message } => {
                    panic!("{}:{}: a trap occurred: {}", name, line, message);
                }
            },
            CommandKind::AssertReturn { action, expected } => {
                match instances.perform_action(isa, action)? {
                    ActionOutcome::Returned { values } => {
                        for (v, e) in values.iter().zip(expected.iter()) {
                            match *e {
                                script::Value::I32(x) => {
                                    assert_eq!(x, v.unwrap_i32(), "at {}:{}", name, line)
                                }
                                script::Value::I64(x) => {
                                    assert_eq!(x, v.unwrap_i64(), "at {}:{}", name, line)
                                }
                                script::Value::F32(x) => {
                                    assert_eq!(x.to_bits(), v.unwrap_f32(), "at {}:{}", name, line)
                                }
                                script::Value::F64(x) => {
                                    assert_eq!(x.to_bits(), v.unwrap_f64(), "at {}:{}", name, line)
                                }
                            };
                        }
                    }
                    ActionOutcome::Trapped { message } => {
                        panic!(
                            "{}:{}: expected normal return, but a trap occurred: {}",
                            name, line, message
                        );
                    }
                }
            }
            CommandKind::AssertTrap { action, message } => {
                match instances.perform_action(isa, action)? {
                    ActionOutcome::Returned { values } => panic!(
                        "{}:{}: expected trap, but invoke returned with {:?}",
                        name, line, values
                    ),
                    ActionOutcome::Trapped {
                        message: trap_message,
                    } => {
                        println!(
                            "{}:{}: TODO: Check the assert_trap message: expected {}, got {}",
                            name, line, message, trap_message
                        );
                    }
                }
            }
            CommandKind::AssertExhaustion { action } => {
                match instances.perform_action(isa, action)? {
                    ActionOutcome::Returned { values } => panic!(
                        "{}:{}: expected exhaustion, but invoke returned with {:?}",
                        name, line, values
                    ),
                    ActionOutcome::Trapped { message } => {
                        println!(
                            "{}:{}: TODO: Check the assert_exhaustion message: {}",
                            name, line, message
                        );
                    }
                }
            }
            CommandKind::AssertReturnCanonicalNan { action } => {
                match instances.perform_action(isa, action)? {
                    ActionOutcome::Returned { values } => {
                        for v in values.iter() {
                            match v {
                                Value::I32(_) | Value::I64(_) => {
                                    panic!("unexpected integer type in NaN test");
                                }
                                Value::F32(x) => assert_eq!(
                                    x & 0x7fffffff,
                                    0x7fc00000,
                                    "expected canonical NaN at {}:{}",
                                    name,
                                    line
                                ),
                                Value::F64(x) => assert_eq!(
                                    x & 0x7fffffffffffffff,
                                    0x7ff8000000000000,
                                    "expected canonical NaN at {}:{}",
                                    name,
                                    line
                                ),
                            };
                        }
                    }
                    ActionOutcome::Trapped { message } => {
                        panic!(
                            "{}:{}: expected canonical NaN return, but a trap occurred: {}",
                            name, line, message
                        );
                    }
                }
            }
            CommandKind::AssertReturnArithmeticNan { action } => {
                match instances.perform_action(isa, action)? {
                    ActionOutcome::Returned { values } => {
                        for v in values.iter() {
                            match v {
                                Value::I32(_) | Value::I64(_) => {
                                    panic!("unexpected integer type in NaN test");
                                }
                                Value::F32(x) => assert_eq!(
                                    x & 0x00400000,
                                    0x00400000,
                                    "expected arithmetic NaN at {}:{}",
                                    name,
                                    line
                                ),
                                Value::F64(x) => assert_eq!(
                                    x & 0x0008000000000000,
                                    0x0008000000000000,
                                    "expected arithmetic NaN at {}:{}",
                                    name,
                                    line
                                ),
                            };
                        }
                    }
                    ActionOutcome::Trapped { message } => {
                        panic!(
                            "{}:{}: expected canonical NaN return, but a trap occurred: {}",
                            name, line, message
                        );
                    }
                }
            }
            CommandKind::AssertInvalid {
                module: _module,
                message: _message,
            } => {
                println!("{}:{}: TODO: Implement assert_invalid", name, line);
            }
            CommandKind::AssertMalformed {
                module: _module,
                message: _message,
            } => {
                println!("{}:{}: TODO: Implement assert_malformed", name, line);
            }
            CommandKind::AssertUninstantiable { module, message } => {
                let _err = instances
                    .define_unnamed_module(isa, module)
                    .expect_err(&format!(
                        "{}:{}: uninstantiable module was successfully instantiated",
                        name, line
                    ));
                println!(
                    "{}:{}: TODO: Check the assert_uninstantiable message: {}",
                    name, line, message
                );
            }
            CommandKind::AssertUnlinkable { module, message } => {
                let _err = instances
                    .define_unnamed_module(isa, module)
                    .expect_err(&format!(
                        "{}:{}: unlinkable module was successfully linked",
                        name, line
                    ));
                println!(
                    "{}:{}: TODO: Check the assert_unlinkable message: {}",
                    name, line, message
                );
            }
        }
    }

    Ok(())
}

/// Run a wast script from a file.
pub fn wast_file(path: &Path, isa: &isa::TargetIsa) -> Result<(), String> {
    let wast = read_to_end(path).map_err(|e| e.to_string())?;
    wast_buffer(&path.display().to_string(), isa, &wast)
}

fn read_to_end(path: &Path) -> Result<Vec<u8>, io::Error> {
    let mut buf: Vec<u8> = Vec::new();
    let mut file = fs::File::open(path)?;
    file.read_to_end(&mut buf)?;
    Ok(buf)
}
