extern crate regex;

use std::collections::HashMap;
use regex::Regex;

use crate::parser::{Node, attributes::HtmlAttribute, StartingTag};
use super::{all_html_tags::is_html_tag, helper::CodeHelper, codegen_script::ScriptAndLang};

#[derive(Default)]
pub struct CodegenContext <'a> {
  pub code_helper: CodeHelper<'a>,
  pub components: HashMap<&'a str, String>,
  pub used_imports: u64,
  hoists: Vec<String>,
  is_custom_element: IsCustomElementParam<'a>
}

enum IsCustomElementParamRaw <'a> {
  String(&'a str),
  Regex(&'a str),
  None
}

enum IsCustomElementParam <'a> {
  String(&'a str),
  Regex(Regex),
  None
}

impl Default for IsCustomElementParam<'_> {
  fn default() -> Self {
    Self::None
  }
}

/**
 * Main entry point for the code generation
 */
pub fn compile_sfc(blocks: &[Node]) -> Result<String, i32> {
  let mut template: Option<&Node> = None;
  // let mut template_lang: Option<&str> = None;
  let mut legacy_script: Option<ScriptAndLang> = None;
  let mut setup_script: Option<ScriptAndLang> = None;

  for block in blocks.iter() {
    match block {
      Node::ElementNode { starting_tag, children } => {
        // Extract lang attr, as this is used later
        let lang_attr = starting_tag.attributes.iter().find_map(|attr| match attr {
          HtmlAttribute::Regular { name, value } => {
            if *name == "lang" {
              Some(*value)
            } else {
              None
            }
          },
          _ => None
        });

        // Template is supported if it doesn't have a `lang` attr or has `lang="html"`
        // todo check if we have double templates (do this in analyzer)
        if starting_tag.tag_name == "template" {
          // todo what should the code do?? the unsupported template compilation should be done outside (e.g. Pug)
          match lang_attr {
            None | Some("html") => {},
            _ => return Err(-2)
          }

          // Save the found template
          template = Some(block);
          // template_lang = lang_attr.or(Some("html"));
        }

        // Script is supported if it has empty `lang`, `lang="js"` or `lang="ts"`
        if starting_tag.tag_name == "script" && children.len() > 0 {
          // todo what should the code do?? maybe return ScriptAndLang in a SFC descriptor
          match lang_attr {
            None | Some("js") | Some("ts") => {},
            _ => return Err(-2)
          }

          // We already did the check for children.len() before
          let script_content = match children[0] {
            Node::TextNode(v) => Ok(v),
            _ => Err(-3) // todo more meaningful error?
          }?;

          // Construct the struct that we can pass around
          let script = Some(ScriptAndLang {
            content: script_content,
            lang: lang_attr.unwrap_or("js")
          });

          let is_setup = starting_tag.attributes.iter().any(|attr| match attr {
            HtmlAttribute::Regular { name, .. } => {
              *name == "setup"
            },
            _ => false
          });

          if is_setup {
            setup_script = script
          } else {
            legacy_script = script
          }
        }
      },

      _ => {
        // do what?
      }
    }
  }

  // Check that there is some work to do
  if !(template.is_some() || legacy_script.is_some() || setup_script.is_some()) {
    return Err(-1000); // todo error enums
  }

  // Resulting buffer
  let mut result = String::new();

  // Todo from options
  let is_custom_element_param = IsCustomElementParamRaw::Regex("custom-");
  let is_custom_element_re = match is_custom_element_param {
    IsCustomElementParamRaw::Regex(re) => IsCustomElementParam::Regex(Regex::new(re).expect("Invalid isCustomElement regex")),
    IsCustomElementParamRaw::String(string) => IsCustomElementParam::String(string),
    IsCustomElementParamRaw::None => IsCustomElementParam::None
  };

  /* Create the context */
  let mut ctx: CodegenContext = Default::default();
  ctx.is_custom_element = is_custom_element_re;

  // Todo generate imports and hoists first in PROD mode (but this requires a smarter order of compilation)

  // Generate scripts
  ctx.compile_scripts(&mut result, &legacy_script, &setup_script);

  // Generate template
  if let Some(template_node) = template {
    // todo do not allocate extra strings in functions?
    // yet, we still need to hold the results of template render fn to append it later
    let compiled_template = ctx.compile_template(template_node)?;
    result.push_str(&compiled_template);
    ctx.code_helper.newline(&mut result);
    result.push_str("__sfc__.render = render");
  }

  result.push('\n');
  result.push_str("export default __sfc__");

  Ok(result)
}

impl <'a> CodegenContext <'a> {
  pub fn compile_template(self: &mut Self, template: &'a Node) -> Result<String, i32> {
    // Try compiling the template. Indent because this will end up in a body of a function.
    // We first need to compile template before knowing imports, components and hoists
    self.code_helper.indent();
    let compiled_template = self.compile_node(&template);
    self.code_helper.unindent();
  
    // todo do not generate this inside compile_template, as PROD mode puts it to the top
    if let Some(render_fn_return) = compiled_template {
      let mut result = self.generate_imports_string();
      self.code_helper.newline_n(&mut result, 2);

      // Function header
      result.push_str("function render(_ctx, _cache, $props, $setup, $data, $options) {");
      self.code_helper.indent();
  
      // Write components
      self.code_helper.newline(&mut result);
      self.generate_components_string(&mut result);
  
      // Write return statement
      self.code_helper.newline_n(&mut result, 2);
      result.push_str("return ");
      result.push_str(&render_fn_return);
  
      // Closing bracket
      self.code_helper.unindent();
      self.code_helper.newline(&mut result);
      result.push('}');
  
      Ok(result)
    } else {
      Err(-1)
    }
  }

  pub fn compile_node(self: &mut Self, node: &'a Node) -> Option<String> {
    // TODO do not use this function
    // instead rename it and generate the code responsible for `openBlock`, `createElementBlock` (+ handle Fragments)
    // TODO using create_element_vnode is generating wrong code, because a component may be a top-level element
    match node {
      Node::ElementNode { starting_tag, children } => {
        let mut buf = String::new();
        self.create_element_vnode(&mut buf, starting_tag, children);
        Some(buf)
      },
      _ => None
    }
  }

  fn add_to_hoists(self: &mut Self, expression: String) -> String {
    let hoist_index = self.hoists.len() + 1;
    let hoist_identifier = format!("_hoisted_{}", hoist_index);

    // todo add pure in consumer instead or provide a boolean flag to generate it
    let hoist_expr = format!("const {} = /*#__PURE__*/ {}", hoist_identifier, expression);
    self.hoists.push(hoist_expr);

    hoist_identifier
  }

  pub fn is_component(self: &Self, starting_tag: &StartingTag) -> bool {
    // todo use analyzed components? (fields of `components: {}`)

    let tag_name = starting_tag.tag_name;

    let is_html_tag = is_html_tag(tag_name);
    if is_html_tag {
      return false;
    }

    /* Check with isCustomElement */
    let is_custom_element = match &self.is_custom_element {
      IsCustomElementParam::String(string) => tag_name == *string,
      IsCustomElementParam::Regex(re) => re.is_match(tag_name),
      IsCustomElementParam::None => false
    };

    !is_custom_element
  }

  /**
   * The element can be hoisted if it and all of its descendants do not have dynamic attributes
   * <div class="a"><span>text</span></div> => true
   * <button :disabled="isDisabled">text</button> => false
   * <span>{{ text }}</span> => false
   */
  fn can_be_hoisted (self: &Self, node: &Node) -> bool {
    match node {
      Node::ElementNode { starting_tag, children } => { 
        /* Check starting tag */
        if self.is_component(starting_tag) {
          return false;
        }

        let has_any_dynamic_attr = starting_tag.attributes.iter().any(|it| {
          match it {
            HtmlAttribute::Regular { .. } => false,
            HtmlAttribute::VDirective { .. } => true
          }
        });

        if has_any_dynamic_attr {
          return false;
        }

        let cannot_be_hoisted = children.iter().any(|it| !self.can_be_hoisted(&it));

        return !cannot_be_hoisted;
      },

      Node::TextNode(_) => {
        return true
      },

      Node::DynamicExpression(_) => {
        return false
      },

      Node::CommentNode(_) => {
        return false;
      }
    };
  }
}
