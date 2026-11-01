//! Rewriting the names in one file so they can live beside every other file's.
//!
//! Expansion produces a single flat program, so two modules that both define
//! `kick` have to end up with two different names in it. Every definition a
//! module makes is filed under `<module>::<name>`, and every mention of it —
//! wherever it was written, and however it was written — is rewritten to that.
//!
//! The whole difficulty is knowing what is a mention. A parameter called `kick`
//! is not the imported `kick`, and neither is a `let` that shadows it, so this
//! walk carries the same scope stack the lowerer will: names bound by a `fn`'s
//! parameters, a `let`, a block's statements, or a `for` are left alone inside
//! the body they cover.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::parser::parser::{Arg, Expr, Ident, Pattern, SwyncItem, Statement};
use crate::samples::LOAD;

/// What the names written in one file mean everywhere else.
pub struct Scope<'a> {
    /// Plain name → the name its definition is filed under in the program.
    /// Holds both what the file imported and what it defines itself.
    names: &'a HashMap<String, String>,
    /// The head of a qualified name → the prefix that module's definitions
    /// carry. `drums` → `lib::drums`, for `use lib::drums`.
    modules: &'a HashMap<String, String>,
    /// The filed names that belong to an enum.
    ///
    /// `Scale.major` parses as a chain — the same shape as `xs.rev` — so without
    /// this the walk would treat `major` as a mention and rewrite it to whatever
    /// the file has under that name. It usually has nothing, which is why this
    /// only bites when an unrelated `major` is in scope: the enum member would
    /// silently become a call to it, in a file that reads correctly.
    enums: &'a HashSet<String>,
    /// Names bound by the code being walked, innermost last. These shadow
    /// everything above: a parameter is not an import.
    locals: Vec<HashSet<String>>,
    /// The folder this file's own `load` paths are relative to, when they have
    /// to be rewritten to survive the move into the program. See
    /// [`Scope::relocating_samples`].
    samples_dir: Option<&'a Path>,
}

impl<'a> Scope<'a> {
    pub fn new(
        names: &'a HashMap<String, String>,
        modules: &'a HashMap<String, String>,
        enums: &'a HashSet<String>,
    ) -> Scope<'a> {
        Scope { names, modules, enums, locals: Vec::new(), samples_dir: None }
    }

    /// Also rewrite every relative `load` path to `dir`, which is the folder
    /// the file being walked sits in.
    ///
    /// A module's definitions are lifted into a program that may be anywhere,
    /// and the paths in them are read once the lifting is over — so a path left
    /// as written would be resolved against the wrong folder, and a module that
    /// plays a sample would only work from the folder it happened to be
    /// imported from. Rewriting here is the one place that still knows which
    /// file the path came out of.
    ///
    /// The program's own paths are left alone: nothing has moved them, and
    /// they are what its errors quote back.
    pub fn relocating_samples(mut self, dir: &'a Path) -> Scope<'a> {
        self.samples_dir = Some(dir);
        self
    }

    fn push(&mut self, bound: impl IntoIterator<Item = String>) {
        self.locals.push(bound.into_iter().collect());
    }

    fn pop(&mut self) {
        self.locals.pop();
    }

    fn shadowed(&self, name: &str) -> bool {
        self.locals.iter().any(|scope| scope.contains(name))
    }

    /// What a written name refers to, or `None` when it refers to itself — a
    /// builtin, a parameter, a drawn pattern, or this file's own definition.
    fn resolve(&self, written: &str) -> Result<Option<String>, String> {
        // A qualified name can be neither shadowed nor local: nothing but a
        // `use` can put a `::` into scope.
        if let Some((head, rest)) = written.split_once("::") {
            let Some(prefix) = self.modules.get(head) else {
                return Err(format!(
                    "no module named `{head}` here — add `use {head}` at the top of \
                     the file, or drop the `{head}::` from `{written}`"
                ));
            };
            return Ok(Some(format!("{prefix}::{rest}")));
        }

        if self.shadowed(written) {
            return Ok(None);
        }

        Ok(self.names.get(written).cloned())
    }

    fn ident(&self, id: &mut Ident) -> Result<(), String> {
        if let Some(name) = self.resolve(&id.0)? {
            id.0 = name;
        }
        Ok(())
    }

    /// Rewrite one top-level item's body. The item's own name is the caller's
    /// business: only it knows whether this file is a module.
    pub fn item(&mut self, item: &mut SwyncItem) -> Result<(), String> {
        match item {
            SwyncItem::Function { params, body, .. } => {
                // Every parameter at once, ahead of the defaults: a default may
                // read an earlier parameter, and none of them is ever an import.
                self.push(params.iter().map(|p| p.name.0.clone()));
                let result = (|| {
                    for param in params.iter_mut() {
                        if let Some(default) = &mut param.default {
                            self.expr(default)?;
                        }
                    }
                    self.expr(body)
                })();
                self.pop();
                result
            }
            SwyncItem::Let { value, .. } => self.expr(value),
            // A member's name is not a mention of anything — it is reached
            // through the enum and nowhere else — so only the values are walked.
            SwyncItem::Enum { members, .. } => {
                for member in members.iter_mut() {
                    if let Some(value) = &mut member.value {
                        self.expr(value)?;
                    }
                }
                Ok(())
            }
            SwyncItem::Expr(e) => self.expr(e),
            SwyncItem::Call { func, args } => {
                self.ident(func)?;
                self.args(args)
            }
            // Gone by the time anything is renamed: a `use` is what built the
            // tables this walk reads.
            SwyncItem::Use(_) => Ok(()),
        }
    }

    /// Point one `load`'s path at the folder the file writing it sits in.
    ///
    /// The shape checked here is the shape [`crate::samples`] looks for — one
    /// argument, a literal — because anything else is already a path it cannot
    /// find, and rewriting it would only change which error it is.
    fn relocate(&self, args: &mut [Arg]) {
        let Some(dir) = self.samples_dir else { return };
        let [arg] = args else { return };
        let Expr::Str(written) = &mut arg.value else { return };
        // The two answers `samples::resolve` gives without consulting a folder:
        // an absolute path is already one, and an empty path is an error whose
        // wording is better than anything a join would leave.
        if written.is_empty() || Path::new(written.as_str()).is_absolute() {
            return;
        }
        *written = dir.join(written.as_str()).to_string_lossy().into_owned();
    }

    /// Whether the left of a dot is an enum, and so whether the right of it is a
    /// member rather than a name.
    ///
    /// Asked *after* the left side has been resolved, so the name compared here
    /// is the one the definition is filed under — which is the only spelling
    /// two files could agree on. A shadowed name resolves to itself and is
    /// therefore not an enum, which is the right answer: a parameter called
    /// `Scale` is not the enum, and `Scale.major` inside that function is a
    /// method call on it.
    fn names_an_enum(&self, lhs: &Expr) -> bool {
        let Expr::Var(name) = lhs else { return false };
        !self.shadowed(&name.0) && self.enums.contains(&name.0)
    }

    /// Rewrite the names inside one pattern that refer to something else.
    ///
    /// There are two: the enum a member is reached through, and whatever a
    /// range's ends read. Everything else a pattern holds is written rather
    /// than read.
    fn pattern(&mut self, pattern: &mut Pattern) -> Result<(), String> {
        match pattern {
            Pattern::Member { ty, .. } => self.ident(ty),
            Pattern::Range { lo, hi } => {
                self.expr(lo)?;
                self.expr(hi)
            }
            Pattern::List(items) | Pattern::Or(items) => {
                for item in items.iter_mut() {
                    self.pattern(item)?;
                }
                Ok(())
            }
            Pattern::Wild
            | Pattern::Num(_)
            | Pattern::Name(_)
            | Pattern::Rest
            | Pattern::Trigger => Ok(()),
        }
    }

    fn args(&mut self, args: &mut [Arg]) -> Result<(), String> {
        for arg in args {
            // Only the value. An argument's name is a parameter of the callee
            // — or a lane of an instrument — and belongs to whatever it names.
            self.expr(&mut arg.value)?;
        }
        Ok(())
    }

    fn expr(&mut self, expr: &mut Expr) -> Result<(), String> {
        match expr {
            Expr::Var(name) => self.ident(name),

            Expr::Call { func, args } => {
                let names_a_file = func.0 == LOAD;
                self.ident(func)?;
                // Still `load` after resolving, so still the builtin: a file
                // that defines a `load` of its own is not naming a path here,
                // and its argument is nobody's business but the lowerer's.
                if names_a_file && func.0 == LOAD {
                    self.relocate(args);
                }
                self.args(args)
            }

            // `Scale.major` is this shape, and so is `xs.rev`. The two want
            // opposite things from the right-hand side: `rev` may be an imported
            // function and has to be rewritten, while `major` is a member and
            // must not be. Which one this is can only be told from the left, so
            // that side is resolved first and the answer decides the other.
            Expr::Chain { lhs, rhs } => {
                self.expr(lhs)?;
                if self.names_an_enum(lhs) {
                    return Ok(());
                }
                self.expr(rhs)
            }

            Expr::Add { lhs, rhs }
            | Expr::Sub { lhs, rhs }
            | Expr::Mul { lhs, rhs }
            | Expr::Div { lhs, rhs }
            | Expr::Rem { lhs, rhs }
            | Expr::Cmp { lhs, rhs, .. } => {
                self.expr(lhs)?;
                self.expr(rhs)
            }

            Expr::Range { lo, hi } => {
                self.expr(lo)?;
                self.expr(hi)
            }

            Expr::Index { base, index } => {
                self.expr(base)?;
                self.expr(index)
            }

            Expr::Neg { expr } => self.expr(expr),
            Expr::Quote { expr } => self.expr(expr),

            Expr::List(items) => {
                for item in items {
                    self.expr(&mut item.value)?;
                    // A length is an expression, so it may name an import too.
                    if let Some(length) = &mut item.length {
                        self.expr(length)?;
                    }
                }
                Ok(())
            }

            Expr::If { cond, then, otherwise } => {
                self.expr(cond)?;
                self.expr(then)?;
                match otherwise {
                    Some(otherwise) => self.expr(otherwise),
                    None => Ok(()),
                }
            }

            // Each arm gets its own scope, holding what its pattern bound —
            // and the guard is inside it as well as the body, because that is
            // where the lowerer reads it.
            //
            // A pattern's own names are never mentions. A binding is a name
            // being written, and the rest — a note, a written value, a
            // member — is reached through a table or through the enum rather
            // than through the file's imports. Only the *type* of a member is
            // a mention: `Section` in `Section.verse` is the same `Section` a
            // `use` brought in, and the member after it is not, which is
            // exactly the split `names_an_enum` makes for a chain.
            Expr::Match { subject, arms } => {
                self.expr(subject)?;
                for arm in arms.iter_mut() {
                    self.pattern(&mut arm.pattern)?;
                    let mut bound = Vec::new();
                    arm.pattern.bindings(&mut bound);
                    self.push(bound.into_iter().map(|id| id.0));
                    let result = (|| {
                        if let Some(guard) = &mut arm.guard {
                            self.expr(guard)?;
                        }
                        self.expr(&mut arm.body)
                    })();
                    self.pop();
                    result?;
                }
                Ok(())
            }

            // The value is outside the binding, the body inside it: `let a = a`
            // reads the outer `a`, exactly as the lowerer evaluates it.
            Expr::Let { name, value, body } => {
                self.expr(value)?;
                self.push([name.0.clone()]);
                let result = self.expr(body);
                self.pop();
                result
            }

            // The `;` length is inside the loop's scope like the body, because
            // that is where the lowerer reads it — a length may be computed
            // from `var` as freely as the step it measures.
            Expr::For { var, iter, body, length } => {
                self.expr(iter)?;
                self.push([var.0.clone()]);
                let result = self.expr(body).and_then(|()| match length {
                    Some(e) => self.expr(e),
                    None => Ok(()),
                });
                self.pop();
                result
            }

            // A block's `let`s come into scope one at a time, so each one's
            // value still sees what was written above it and no more.
            Expr::Block { stmts, tail } => {
                self.push([]);
                let result = (|| {
                    for stmt in stmts.iter_mut() {
                        match stmt {
                            Statement::Expr(e) => self.expr(e)?,
                            Statement::Let { name, value } => {
                                self.expr(value)?;
                                let bound = name.0.clone();
                                self.locals
                                    .last_mut()
                                    .expect("just pushed")
                                    .insert(bound);
                            }
                        }
                    }
                    self.expr(tail)
                })();
                self.pop();
                result
            }

            // A string is a path to a file on disk, not a name in a module, so
            // qualifying an import never touches one.
            Expr::Num(_) | Expr::Str(_) | Expr::Rest | Expr::Trigger => Ok(()),
        }
    }
}
