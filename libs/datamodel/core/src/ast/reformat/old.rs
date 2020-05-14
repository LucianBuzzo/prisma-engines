use crate::{ast::parser::*, ast::renderer::*};
use pest::Parser;

// We have to use RefCell as rust cannot
// do multiple mutable borrows inside a match statement.
use std::cell::RefCell;

type Token<'a> = pest::iterators::Pair<'a, Rule>;

pub struct ReformatterOld<'a> {
    input: &'a str,
    missing_fields: Result<Vec<MissingField>, crate::error::ErrorCollection>,
}

fn count_lines(text: &str) -> usize {
    bytecount::count(text.as_bytes(), b'\n')
}

fn newlines(target: &mut dyn LineWriteable, text: &str, _identifier: &str) {
    for _i in 0..count_lines(text) {
        // target.write(&format!("{}{}", i, identifier));
        target.end_line();
    }
}

fn comment(target: &mut dyn LineWriteable, comment_text: &str) {
    let trimmed = if comment_text.ends_with("\n") {
        &comment_text[0..comment_text.len() - 1] // slice away line break.
    } else {
        &comment_text
    };

    if !target.line_empty() {
        // Prefix with whitespace seperator.
        target.write(trimmed);
    } else {
        target.write(trimmed);
    }
    target.end_line();
}

trait TokenExtensions {
    fn is_top_level_element(&self) -> bool;
}

impl TokenExtensions for Token<'_> {
    fn is_top_level_element(&self) -> bool {
        match self.as_rule() {
            Rule::model_declaration => true,
            Rule::enum_declaration => true,
            Rule::source_block => true,
            Rule::generator_block => true,
            Rule::type_declaration => true,
            _ => false,
        }
    }
}

impl<'a> ReformatterOld<'a> {
    pub fn new(input: &'a str) -> Self {
        let missing_fields = Self::find_all_missing_fields(&input);
        ReformatterOld { input, missing_fields }
    }

    // this finds all auto generated fields, that are added during auto generation AND are missing from the original input.
    fn find_all_missing_fields(schema_string: &str) -> Result<Vec<MissingField>, crate::error::ErrorCollection> {
        let schema_ast = crate::parse_schema_ast(&schema_string)?;
        let datamodel = crate::lift_ast(&schema_ast)?;
        let lowerer = crate::validator::LowerDmlToAst::new();
        let mut result = Vec::new();

        for model in datamodel.models() {
            let ast_model = schema_ast.find_model(&model.name).unwrap();

            for field in model.fields() {
                if ast_model.fields.iter().find(|f| &f.name.name == &field.name).is_none() {
                    let ast_field = lowerer.lower_field(&field, &datamodel)?;

                    result.push(MissingField {
                        model: model.name.clone(),
                        field: ast_field,
                    });
                }
            }
        }

        Ok(result)
    }

    pub fn reformat_to(&self, output: &mut dyn std::io::Write, ident_width: usize) {
        let mut ast = PrismaDatamodelParser::parse(Rule::datamodel, self.input).unwrap(); // TODO: Handle error.
        let mut top_formatter = RefCell::new(Renderer::new(output, ident_width));
        self.reformat_top(&mut top_formatter, &ast.next().unwrap());
    }

    fn reformat_top(&self, target: &mut RefCell<Renderer>, token: &Token) {
        let mut types_table = TableFormat::new();
        let mut types_mode = false;
        let mut seen_at_least_one_top_level_element = false;

        for current in token.clone().into_inner() {
            match current.as_rule() {
                Rule::WHITESPACE => {}
                Rule::doc_comment | Rule::doc_comment_and_new_line => {}
                Rule::type_declaration => {
                    types_mode = true;
                }
                _ => {
                    if types_mode {
                        types_mode = false;
                        // For all other ones, reset types_table.
                        types_table.render(target.get_mut());
                        types_table = TableFormat::new();
                        target.get_mut().maybe_end_line();
                    }
                }
            };

            // new line handling outside of blocks:
            // * fold multiple new lines between blocks into one
            // * all new lines before the first block get removed
            if current.is_top_level_element() {
                // separate top level elements with new lines
                if seen_at_least_one_top_level_element {
                    //                    println!("rendering newline");
                    target.get_mut().write("\n");
                }
                seen_at_least_one_top_level_element = true;
            }

            //            println!("top level: |{:?}|", current.as_str());

            match current.as_rule() {
                Rule::doc_comment | Rule::doc_comment_and_new_line => {
                    if types_mode {
                        comment(&mut types_table.interleave_writer(), current.as_str());
                    } else {
                        comment(target.get_mut(), current.as_str());
                    }
                }
                Rule::model_declaration => self.reformat_model(target.get_mut(), &current),
                Rule::enum_declaration => Self::reformat_enum(target, &current),
                Rule::source_block => self.reformat_datasource(target.get_mut(), &current),
                Rule::generator_block => self.reformat_generator(target.get_mut(), &current),
                Rule::type_declaration => {
                    if !types_mode {
                        panic!("Renderer not in type mode.");
                    }
                    Self::reformat_type_declaration(&mut types_table, &current);
                }
                Rule::EOI => {}
                Rule::WHITESPACE => {} // we don't want to retain whitespace at the top level. Just within the blocks.
                Rule::NEWLINE => {} // Do not render user provided newlines. We have a strong opinionation about new lines on the top level.
                _ => Self::reformat_generic_token(target.get_mut(), &current, false),
            }
        }

        // FLUSH IT. Otherwise pending new lines do not get rendered.
        target.get_mut().write("");
    }

    fn reformat_datasource(&self, target: &mut Renderer, token: &Token) {
        self.reformat_block_element(
            "datasource",
            target,
            token,
            Box::new(|table, _, token| match token.as_rule() {
                Rule::DATASOURCE_KEYWORD => {}
                Rule::key_value => Self::reformat_key_value(table, &token),
                _ => Self::reformat_generic_token(table, &token, true),
            }),
        );
    }

    fn reformat_generator(&self, target: &mut Renderer, token: &Token) {
        self.reformat_block_element(
            "generator",
            target,
            token,
            Box::new(|table, _, token| {
                //
                match token.as_rule() {
                    Rule::GENERATOR_KEYWORD => {}
                    Rule::key_value => Self::reformat_key_value(table, &token),
                    _ => Self::reformat_generic_token(table, &token, true),
                }
            }),
        );
    }

    fn reformat_key_value(target: &mut TableFormat, token: &Token) {
        for current in token.clone().into_inner() {
            match current.as_rule() {
                Rule::non_empty_identifier | Rule::maybe_empty_identifier => {
                    target.write(current.as_str());
                    target.write("=");
                }
                Rule::expression => {
                    Self::reformat_expression(&mut target.column_locked_writer_for(2), &current);
                }
                Rule::WHITESPACE => {}
                Rule::doc_comment | Rule::doc_comment_and_new_line => {
                    panic!("Comments inside config key/value not supported yet.")
                }
                _ => Self::reformat_generic_token(target, &current, false),
            }
        }
    }

    fn reformat_model(&self, target: &mut Renderer, token: &Token) {
        self.reformat_block_element_internal(
            "model",
            target,
            &token,
            Box::new(|table, renderer, token| {
                match token.as_rule() {
                    Rule::MODEL_KEYWORD => {}
                    Rule::directive => {
                        // model level Directives reset the table. -> .render() does that
                        table.render(renderer);
                        Self::reformat_directive(renderer, &token, "@@");
                        //                        table.end_line();
                    }
                    Rule::field_declaration => Self::reformat_field(table, &token),
                    _ => Self::reformat_generic_token(table, &token, true),
                }
            }),
            Box::new(|table, _, model_name| {
                // TODO: what is the right thing to do on error?
                if let Ok(missing_fields) = self.missing_fields.as_ref() {
                    for missing_back_relation_field in missing_fields.iter() {
                        if missing_back_relation_field.model.as_str() == model_name {
                            Renderer::render_field(table, &missing_back_relation_field.field, false);
                        }
                    }
                }
            }),
        );
    }

    fn reformat_block_element(
        &self,
        block_type: &'static str,
        renderer: &'a mut Renderer,
        token: &'a Token,
        the_fn: Box<dyn Fn(&mut TableFormat, &mut Renderer, &Token) -> () + 'a>,
    ) {
        self.reformat_block_element_internal(block_type, renderer, token, the_fn, {
            // a no op
            Box::new(|_, _, _| ())
        })
    }

    fn reformat_block_element_internal(
        &self,
        block_type: &'static str,
        renderer: &'a mut Renderer,
        token: &'a Token,
        the_fn: Box<dyn Fn(&mut TableFormat, &mut Renderer, &Token) -> () + 'a>,
        after_fn: Box<dyn Fn(&mut TableFormat, &mut Renderer, &str) -> () + 'a>,
    ) {
        let mut table = TableFormat::new();
        let mut block_name = "";
        let mut render_new_lines = false;

        for current in token.clone().into_inner() {
            //            println!("block: {:?}", current.as_str());
            match current.as_rule() {
                Rule::BLOCK_OPEN => {
                    render_new_lines = true; // do not render newlines before the block
                }
                Rule::BLOCK_CLOSE => {}

                Rule::non_empty_identifier | Rule::maybe_empty_identifier => {
                    // Begin.
                    block_name = current.as_str();
                    renderer.write(&format!("{} {} {{", block_type, block_name));
                    renderer.maybe_end_line();
                    renderer.indent_up();
                }
                // Doc comments are to be placed OUTSIDE of table block.
                Rule::doc_comment | Rule::doc_comment_and_new_line => comment(renderer, current.as_str()),
                Rule::NEWLINE => {
                    if render_new_lines {
                        // Reset the table layout on a newline.
                        table.render(renderer);
                        table = TableFormat::new();
                        renderer.end_line();
                    }
                }
                Rule::doc_comment | Rule::doc_comment_and_new_line => {
                    comment(&mut table.interleave_writer(), current.as_str())
                }
                _ => the_fn(&mut table, renderer, &current),
            }
        }

        after_fn(&mut table, renderer, block_name);

        // End.
        table.render(renderer);
        renderer.indent_down();
        renderer.write("}");
        renderer.maybe_end_line();
    }

    // TODO: This is very similar to model reformating.
    fn reformat_enum(target: &mut RefCell<Renderer>, token: &Token) {
        let mut table = TableFormat::new();
        // Switch to skip whitespace in 'enum xxxx {'
        let mut skip_whitespace = false;

        for current in token.clone().into_inner() {
            match current.as_rule() {
                Rule::ENUM_KEYWORD => {
                    skip_whitespace = true;
                }
                Rule::BLOCK_OPEN => {
                    skip_whitespace = false;
                }
                Rule::BLOCK_CLOSE => {}

                Rule::non_empty_identifier | Rule::maybe_empty_identifier => {
                    // Begin.
                    target.get_mut().write(&format!("enum {} {{", current.as_str()));
                    target.get_mut().maybe_end_line();
                    target.get_mut().indent_up();
                }
                Rule::directive => {
                    table.render(target.get_mut());
                    table = TableFormat::new();
                    Self::reformat_directive(target.get_mut(), &current, "@@");
                }
                Rule::enum_field_declaration => Self::reformat_enum_entry(&mut table, &current),
                // Doc comments are to be placed OUTSIDE of table block.
                Rule::doc_comment | Rule::doc_comment_and_new_line => comment(target.get_mut(), current.as_str()),
                Rule::WHITESPACE => {
                    if !skip_whitespace {
                        let lines = count_lines(current.as_str());

                        if lines > 1 || (lines == 1 && table.line_empty()) {
                            // Reset the table layout on more than one newline.
                            table.render(target.get_mut());
                            table = TableFormat::new();
                        }

                        newlines(&mut table, current.as_str(), "m");
                    }
                }
                Rule::doc_comment | Rule::doc_comment_and_new_line => {
                    comment(&mut table.interleave_writer(), current.as_str())
                }
                _ => Self::reformat_generic_token(&mut table, &current, true),
            }
        }

        // End.
        table.render(target.get_mut());
        target.get_mut().indent_down();
        //        target.get_mut().end_line();
        target.get_mut().write("}");
        target.get_mut().maybe_end_line();
    }

    fn reformat_enum_entry(target: &mut TableFormat, token: &Token) {
        for current in token.clone().into_inner() {
            match current.as_rule() {
                Rule::non_empty_identifier => target.write(current.as_str()),
                Rule::WHITESPACE => {}
                _ => Self::reformat_generic_token(target, &current, false),
            }
        }
    }

    // TODO: do we still need skip_whitespace??
    fn reformat_generic_token(target: &mut dyn LineWriteable, token: &Token, skip_whitespace: bool) {
        //        println!("generic token: |{:?}|", token.as_str());
        match token.as_rule() {
            Rule::NEWLINE => target.end_line(),
            Rule::doc_comment_and_new_line => comment(target, token.as_str()),
            Rule::WHITESPACE => {
                //                newlines(target, token.as_str(), "m");
                if !skip_whitespace {
                    target.write(token.as_str());
                }
            }
            Rule::CATCH_ALL => {
                target.write(token.as_str());
            }
            _ => unreachable!(
                "Encountered impossible enum declaration during parsing: {:?}",
                token.clone().tokens()
            ),
        }
    }

    fn reformat_field(target: &mut TableFormat, token: &Token) {
        let mut identifier = None;
        let mut directives_started = false;

        for current in token.clone().into_inner() {
            match current.as_rule() {
                Rule::non_empty_identifier | Rule::maybe_empty_identifier => {
                    identifier = Some(String::from(current.as_str()))
                }
                Rule::field_type => {
                    target.write(&identifier.clone().expect("Unknown field identifier."));
                    target.write(&Self::reformat_field_type(&current));
                }
                Rule::directive => {
                    directives_started = true;
                    Self::reformat_directive(&mut target.column_locked_writer_for(2), &current, "@")
                }
                Rule::doc_comment | Rule::doc_comment_and_new_line => {
                    comment(&mut target.interleave_writer(), current.as_str())
                }
                Rule::doc_comment | Rule::doc_comment_and_new_line => {
                    if directives_started {
                        comment(&mut target.column_locked_writer_for(2), current.as_str());
                    } else {
                        comment(target, current.as_str());
                    }
                }
                Rule::WHITESPACE => newlines(target, current.as_str(), "f"),
                _ => Self::reformat_generic_token(target, &current, false),
            }
        }

        target.maybe_end_line();
    }

    fn reformat_type_declaration(target: &mut TableFormat, token: &Token) {
        let mut identifier = None;
        let mut directives_started = false;

        for current in token.clone().into_inner() {
            match current.as_rule() {
                Rule::TYPE_KEYWORD => {}
                Rule::non_empty_identifier | Rule::maybe_empty_identifier => {
                    identifier = Some(String::from(current.as_str()))
                }
                Rule::base_type => {
                    target.write("type");
                    target.write(&identifier.clone().expect("Unknown field identifier."));
                    target.write("=");
                    target.write(&Self::get_identifier(&current));
                }
                Rule::directive => {
                    directives_started = true;
                    Self::reformat_directive(&mut target.column_locked_writer_for(4), &current, "@");
                }
                Rule::doc_comment | Rule::doc_comment_and_new_line => {
                    comment(&mut target.interleave_writer(), current.as_str())
                }
                Rule::doc_comment | Rule::doc_comment_and_new_line => {
                    if directives_started {
                        comment(&mut target.column_locked_writer_for(4), current.as_str());
                    } else {
                        comment(&mut target.interleave_writer(), current.as_str());
                    }
                }
                Rule::WHITESPACE => newlines(target, current.as_str(), "t"),
                _ => unreachable!(
                    "Encounterd impossible custom type during parsing: {:?}",
                    current.tokens()
                ),
            }
        }

        target.maybe_end_line();
    }

    fn reformat_field_type(token: &Token) -> String {
        let mut builder = StringBuilder::new();

        for current in token.clone().into_inner() {
            builder.write(&Self::get_identifier(&current));
            match current.as_rule() {
                Rule::optional_type => builder.write("?"),
                Rule::base_type => {}
                Rule::list_type => builder.write("[]"),
                _ => unreachable!(
                    "Encounterd impossible field type during parsing: {:?}",
                    current.tokens()
                ),
            }
        }

        builder.to_string()
    }

    fn get_identifier(token: &Token) -> String {
        for current in token.clone().into_inner() {
            if let Rule::non_empty_identifier | Rule::maybe_empty_identifier = current.as_rule() {
                return current.as_str().to_string();
            }
        }

        panic!("No identifier found.")
    }

    fn reformat_directive(target: &mut dyn LineWriteable, token: &Token, owl: &str) {
        for current in token.clone().into_inner() {
            match current.as_rule() {
                Rule::directive_name => {
                    // Begin
                    if !target.line_empty() {
                        target.write(" ");
                    }
                    target.write(owl);
                    target.write(current.as_str());
                }
                Rule::WHITESPACE => {}
                Rule::doc_comment | Rule::doc_comment_and_new_line => {
                    panic!("Comments inside attributes not supported yet.")
                }
                Rule::directive_arguments => Self::reformat_directive_args(target, &current),
                _ => unreachable!("Encounterd impossible directive during parsing: {:?}", current.tokens()),
            }
        }
    }

    fn reformat_directive_args(target: &mut dyn LineWriteable, token: &Token) {
        let mut builder = StringBuilder::new();

        for current in token.clone().into_inner() {
            match current.as_rule() {
                // This is a named arg.
                Rule::argument => {
                    if !builder.line_empty() {
                        builder.write(", ");
                    }
                    Self::reformat_directive_arg(&mut builder, &current);
                }
                // This is a an unnamed arg.
                Rule::argument_value => {
                    if !builder.line_empty() {
                        builder.write(", ");
                    }
                    Self::reformat_arg_value(&mut builder, &current);
                }
                Rule::WHITESPACE => {}
                Rule::doc_comment | Rule::doc_comment_and_new_line => {
                    panic!("Comments inside attribute argument list not supported yet.")
                }
                _ => unreachable!(
                    "Encounterd impossible directive argument list during parsing: {:?}",
                    current.tokens()
                ),
            };
        }

        if !builder.line_empty() {
            target.write("(");
            target.write(&builder.to_string());
            target.write(")");
        }
    }

    fn reformat_directive_arg(target: &mut dyn LineWriteable, token: &Token) {
        for current in token.clone().into_inner() {
            match current.as_rule() {
                Rule::argument_name => {
                    target.write(current.as_str());
                    target.write(": ");
                }
                Rule::argument_value => Self::reformat_arg_value(target, &current),
                Rule::WHITESPACE => {}
                Rule::doc_comment | Rule::doc_comment_and_new_line => {
                    panic!("Comments inside attribute argument not supported yet.")
                }
                _ => unreachable!(
                    "Encounterd impossible directive argument during parsing: {:?}",
                    current.tokens()
                ),
            };
        }
    }

    fn reformat_arg_value(target: &mut dyn LineWriteable, token: &Token) {
        for current in token.clone().into_inner() {
            match current.as_rule() {
                Rule::expression => Self::reformat_expression(target, &current),
                Rule::WHITESPACE => {}
                Rule::doc_comment | Rule::doc_comment_and_new_line => {
                    panic!("Comments inside attributes not supported yet.")
                }
                _ => unreachable!(
                    "Encounterd impossible argument value during parsing: {:?}",
                    current.tokens()
                ),
            };
        }
    }

    /// Parses an expression, given a Pest parser token.
    fn reformat_expression(target: &mut dyn LineWriteable, token: &Token) {
        for current in token.clone().into_inner() {
            match current.as_rule() {
                Rule::numeric_literal => target.write(current.as_str()),
                Rule::string_literal => target.write(current.as_str()),
                Rule::boolean_literal => target.write(current.as_str()),
                Rule::constant_literal => target.write(current.as_str()),
                Rule::function => Self::reformat_function_expression(target, &current),
                Rule::array_expression => Self::reformat_array_expression(target, &current),
                Rule::WHITESPACE => {}
                Rule::doc_comment | Rule::doc_comment_and_new_line => {
                    panic!("Comments inside expressions not supported yet.")
                }
                _ => unreachable!("Encounterd impossible literal during parsing: {:?}", current.tokens()),
            }
        }
    }

    fn reformat_array_expression(target: &mut dyn LineWriteable, token: &Token) {
        target.write("[");
        let mut expr_count = 0;

        for current in token.clone().into_inner() {
            match current.as_rule() {
                Rule::expression => {
                    if expr_count > 0 {
                        target.write(", ");
                    }
                    Self::reformat_expression(target, &current);
                    expr_count += 1;
                }
                Rule::WHITESPACE => {}
                Rule::doc_comment | Rule::doc_comment_and_new_line => {
                    panic!("Comments inside expressions not supported yet.")
                }
                _ => unreachable!("Encounterd impossible array during parsing: {:?}", current.tokens()),
            }
        }

        target.write("]");
    }

    fn reformat_function_expression(target: &mut dyn LineWriteable, token: &Token) {
        let mut expr_count = 0;

        for current in token.clone().into_inner() {
            match current.as_rule() {
                Rule::non_empty_identifier | Rule::maybe_empty_identifier => {
                    target.write(current.as_str());
                    target.write("(");
                }
                Rule::argument_value => {
                    if expr_count > 0 {
                        target.write(", ");
                    }
                    Self::reformat_arg_value(target, &current);
                    expr_count += 1;
                }
                Rule::WHITESPACE => {}
                Rule::doc_comment | Rule::doc_comment_and_new_line => {
                    panic!("Comments inside expressions not supported yet.")
                }
                _ => unreachable!("Encounterd impossible function during parsing: {:?}", current.tokens()),
            }
        }

        target.write(")");
    }
}

#[derive(Debug)]
pub struct MissingField {
    pub model: String,
    pub field: crate::ast::Field,
}
