//! Validation of the installer version within the step's `with` mapping.

use super::{INSTALLER_VERSION_INPUT, WhitakerStep, indent_of};

impl WhitakerStep {
    /// Require one direct `with` mapping with one matching installer input.
    pub(super) fn validate_installer_input(&self) -> Result<(), String> {
        let inputs = self.installer_inputs()?;
        if inputs.as_slice() != [INSTALLER_VERSION_INPUT] {
            return Err(format!(
                "expected one configured installer input, found {inputs:?}"
            ));
        }
        Ok(())
    }

    /// Collect direct installer inputs from the sole `with` mapping.
    fn installer_inputs(&self) -> Result<Vec<&str>, String> {
        let is_with = |line: &&str| indent_of(line) == self.field_indent && line.trim() == "with:";
        if self.text.lines().filter(is_with).count() != 1 {
            return Err("expected one with mapping".to_owned());
        }
        let mut lines = self.text.lines();
        lines
            .by_ref()
            .find(is_with)
            .ok_or_else(|| "missing with mapping".to_owned())?;
        Ok(lines
            .take_while(|line| !self.is_field_boundary(line))
            .filter(|line| self.is_installer_input(line))
            .map(str::trim)
            .collect())
    }

    /// Comments and blank lines cannot end the input mapping.
    fn is_field_boundary(&self, line: &str) -> bool {
        let trimmed = line.trim();
        !trimmed.is_empty() && !trimmed.starts_with('#') && indent_of(line) <= self.field_indent
    }

    /// Match only the direct installer input, excluding nested fields and comments.
    fn is_installer_input(&self, line: &str) -> bool {
        indent_of(line) == self.field_indent + 2 && line.trim().starts_with("installer-version:")
    }
}
