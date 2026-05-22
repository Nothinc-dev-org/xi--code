//! Registro de tools disponibles para el agente.

use std::sync::Arc;

use crate::{builtins, Tool};

/// Conjunto de tools que el motor expone al modelo y resuelve por nombre.
#[derive(Clone)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Registro vacío.
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    /// Registro con todas las tools integradas (Fase 3).
    pub fn with_builtins() -> Self {
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(builtins::ReadFile),
            Arc::new(builtins::WriteFile),
            Arc::new(builtins::EditFile),
            Arc::new(builtins::ListDir),
            Arc::new(builtins::Glob),
            Arc::new(builtins::Grep),
            Arc::new(builtins::Bash),
        ];
        Self { tools }
    }

    /// Registra una tool adicional.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.push(tool);
    }

    /// Busca una tool por su nombre.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name)
    }

    /// Itera sobre las tools registradas (para construir la petición al proveedor).
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.tools.iter()
    }

    /// Número de tools registradas.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// `true` si no hay tools registradas.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}
