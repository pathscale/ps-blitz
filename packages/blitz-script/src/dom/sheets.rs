//! Constructable stylesheets use Stylo's parser and shared stylesheet objects.

use boa_engine::object::{
    JsObject,
    builtins::{JsArray, JsPromise, JsProxyBuilder},
};
use boa_engine::{
    Context, Finalize, JsData, JsNativeError, JsResult, JsString, JsValue, NativeFunction, Trace,
    js_string,
};
use style::shared_lock::ToCssWithGuard;
use style::stylesheets::{
    AllowImportRules, CssRule, CssRuleTypes, DocumentStyleSheet, RulesMutateError,
    StylesheetInDocument,
};

use super::{
    define_accessor, define_method, define_value, dom_ctx, js_str, node_id_of_value, this_node_id,
    to_rust_string,
};

const ADOPTED: &str = "__blitz_adopted_stylesheets__";
const ADOPTED_TARGET: &str = "__blitz_adopted_stylesheet_target__";
const OWNER: &str = "__blitz_adopted_stylesheet_owner__";
const RULE_LIST: &str = "__blitz_css_rules__";

#[derive(Clone, Trace, Finalize, JsData)]
struct SheetProtos {
    sheet: JsObject,
    list: JsObject,
    rule: JsObject,
}

#[derive(Trace, Finalize, JsData)]
struct SheetRef {
    #[unsafe_ignore_trace]
    sheet: DocumentStyleSheet,
}

#[derive(Trace, Finalize, JsData)]
struct RuleListRef {
    sheet: JsObject,
}

#[derive(Trace, Finalize, JsData)]
struct RuleRef {
    #[unsafe_ignore_trace]
    rule: CssRule,
    sheet: JsObject,
}

pub(crate) fn init(document: &JsObject, root: &JsObject, context: &mut Context) {
    let sheet = JsObject::with_object_proto(context.intrinsics());
    let list = JsObject::with_object_proto(context.intrinsics());
    let rule = JsObject::with_object_proto(context.intrinsics());
    define_method(&sheet, "replaceSync", 1, replace_sync, context);
    define_method(&sheet, "replace", 1, replace, context);
    define_method(&sheet, "insertRule", 1, insert_rule, context);
    define_method(&sheet, "deleteRule", 1, delete_rule, context);
    define_accessor(&sheet, "cssRules", Some(css_rules), None, context);
    define_method(&list, "item", 1, rule_item, context);
    define_accessor(&rule, "cssText", Some(rule_text), None, context);
    define_accessor(&rule, "type", Some(rule_type), None, context);
    for owner in [document, root] {
        define_accessor(
            owner,
            "adoptedStyleSheets",
            Some(get_adopted),
            Some(set_adopted),
            context,
        );
    }
    context.insert_data(SheetProtos {
        sheet: sheet.clone(),
        list,
        rule,
    });
    context
        .register_global_callable(
            js_string!("CSSStyleSheet"),
            0,
            NativeFunction::from_fn_ptr(constructor),
        )
        .expect("failed to register CSSStyleSheet");
    let constructor = context
        .global_object()
        .get(js_string!("CSSStyleSheet"), context)
        .expect("CSSStyleSheet constructor missing")
        .as_object()
        .expect("CSSStyleSheet constructor is not an object");
    constructor
        .set(js_string!("prototype"), sheet.clone(), true, context)
        .expect("failed to set CSSStyleSheet prototype");
    define_value(&sheet, "constructor", constructor.into(), context);
}

fn constructor(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ctx = dom_ctx(context)?;
    let sheet = ctx.doc.borrow().make_constructed_stylesheet("");
    let proto = context.get_data::<SheetProtos>().unwrap().sheet.clone();
    Ok(JsObject::from_proto_and_data(Some(proto), SheetRef { sheet }).into())
}

fn sheet_of(value: &JsValue) -> JsResult<DocumentStyleSheet> {
    value
        .as_object()
        .and_then(|object| {
            object
                .downcast_ref::<SheetRef>()
                .map(|data| data.sheet.clone())
        })
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("CSSStyleSheet receiver required")
                .into()
        })
}

fn changed(sheet: &DocumentStyleSheet, context: &mut Context) -> JsResult<()> {
    let ctx = dom_ctx(context)?;
    ctx.mutate_doc().constructed_stylesheet_changed(sheet);
    Ok(())
}

fn replace_sync(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let sheet = sheet_of(this)?;
    let text = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let ctx = dom_ctx(context)?;
    let parsed = ctx.doc.borrow().make_constructed_stylesheet(&text);
    let contents = {
        let guard = parsed.0.shared_lock.read();
        parsed.0.contents.read_with(&guard).clone()
    };
    {
        let mut guard = sheet.0.shared_lock.write();
        *sheet.0.contents.write_with(&mut guard) = contents;
    }
    changed(&sheet, context)?;
    Ok(JsValue::undefined())
}

fn replace(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    replace_sync(this, args, context)?;
    Ok(JsPromise::resolve(this.clone(), context)?.into())
}

fn rule_error(error: RulesMutateError) -> boa_engine::JsError {
    let name = match error {
        RulesMutateError::Syntax => "SyntaxError",
        RulesMutateError::IndexSize => "IndexSizeError",
        RulesMutateError::HierarchyRequest => "HierarchyRequestError",
        RulesMutateError::InvalidState => "InvalidStateError",
    };
    JsNativeError::typ().with_message(name).into()
}

fn insert_rule(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let sheet = sheet_of(this)?;
    let text = to_rust_string(args.first().unwrap_or(&JsValue::undefined()), context)?;
    let index = args.get(1).unwrap_or(&JsValue::from(0)).to_u32(context)? as usize;
    let (rules, rule) = {
        let guard = sheet.0.shared_lock.read();
        let contents = sheet.contents(&guard);
        let rules = contents.rules.clone();
        let rule = rules
            .read_with(&guard)
            .parse_rule_for_insert(
                &sheet.0.shared_lock,
                &text,
                contents,
                index,
                CssRuleTypes::from_bits(0),
                None,
                None,
                AllowImportRules::No,
            )
            .map_err(rule_error)?;
        (rules, rule)
    };
    {
        let mut guard = sheet.0.shared_lock.write();
        rules.write_with(&mut guard).0.insert(index, rule);
    }
    changed(&sheet, context)?;
    Ok(JsValue::from(index as u32))
}

fn delete_rule(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let sheet = sheet_of(this)?;
    let index = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_u32(context)? as usize;
    let rules = {
        let guard = sheet.0.shared_lock.read();
        sheet.contents(&guard).rules.clone()
    };
    {
        let mut guard = sheet.0.shared_lock.write();
        rules
            .write_with(&mut guard)
            .remove_rule(index)
            .map_err(rule_error)?;
    }
    changed(&sheet, context)?;
    Ok(JsValue::undefined())
}

fn css_rules(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    sheet_of(this)?;
    let object = this.as_object().unwrap();
    let existing = object.get(JsString::from(RULE_LIST), context)?;
    if !existing.is_undefined() {
        return Ok(existing);
    }
    let proto = context.get_data::<SheetProtos>().unwrap().list.clone();
    let target = JsObject::from_proto_and_data(
        Some(proto),
        RuleListRef {
            sheet: object.clone(),
        },
    );
    let list: JsObject = JsProxyBuilder::new(target)
        .get(rule_list_get)
        .build(context)?
        .into();
    define_value(&object, RULE_LIST, list.clone().into(), context);
    Ok(list.into())
}

fn rule_list_sheet(value: &JsValue) -> JsResult<JsObject> {
    value
        .as_object()
        .and_then(|object| {
            object
                .downcast_ref::<RuleListRef>()
                .map(|data| data.sheet.clone())
        })
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("CSSRuleList receiver required")
                .into()
        })
}

fn rule_at(sheet_object: &JsObject, index: usize, context: &mut Context) -> JsResult<JsValue> {
    let sheet = sheet_of(&sheet_object.clone().into())?;
    let rule = {
        let guard = sheet.0.shared_lock.read();
        sheet.contents(&guard).rules(&guard).get(index).cloned()
    };
    let Some(rule) = rule else {
        return Ok(JsValue::undefined());
    };
    let proto = context.get_data::<SheetProtos>().unwrap().rule.clone();
    Ok(JsObject::from_proto_and_data(
        Some(proto),
        RuleRef {
            rule,
            sheet: sheet_object.clone(),
        },
    )
    .into())
}

fn rule_list_get(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = &args.first().cloned().unwrap_or_default();
    let sheet_object = rule_list_sheet(target)?;
    let key = &args.get(1).cloned().unwrap_or_default();
    if let Some(string) = key.as_string() {
        let key = string.to_std_string_lossy();
        if key == "length" {
            let sheet = sheet_of(&sheet_object.clone().into())?;
            let length = {
                let guard = sheet.0.shared_lock.read();
                sheet.contents(&guard).rules(&guard).len()
            };
            return Ok(JsValue::from(length as u32));
        }
        if let Ok(index) = key.parse::<usize>() {
            return rule_at(&sheet_object, index, context);
        }
        if key == "item" {
            let function = boa_engine::object::FunctionObjectBuilder::new(
                context.realm(),
                NativeFunction::from_copy_closure_with_captures(
                    |_, args, sheet, context| {
                        let index = args
                            .first()
                            .unwrap_or(&JsValue::undefined())
                            .to_u32(context)?;
                        let value = rule_at(sheet, index as usize, context)?;
                        Ok(if value.is_undefined() {
                            JsValue::null()
                        } else {
                            value
                        })
                    },
                    sheet_object,
                ),
            )
            .build();
            return Ok(function.into());
        }
    }
    let object = target.as_object().unwrap();
    object.get(key.to_property_key(context)?, context)
}

fn rule_item(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let sheet = rule_list_sheet(this)?;
    let index = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_u32(context)?;
    let value = rule_at(&sheet, index as usize, context)?;
    Ok(if value.is_undefined() {
        JsValue::null()
    } else {
        value
    })
}

fn rule_text(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let object = this
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("CSSRule receiver required"))?;
    let data = object
        .downcast_ref::<RuleRef>()
        .ok_or_else(|| JsNativeError::typ().with_message("CSSRule receiver required"))?;
    let sheet = sheet_of(&data.sheet.clone().into())?;
    let guard = sheet.0.shared_lock.read();
    Ok(js_str(&data.rule.to_css_string(&guard)))
}

fn rule_type(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let object = this
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("CSSRule receiver required"))?;
    let data = object
        .downcast_ref::<RuleRef>()
        .ok_or_else(|| JsNativeError::typ().with_message("CSSRule receiver required"))?;
    Ok(JsValue::from(data.rule.rule_type() as u32))
}

fn owner_id(this: &JsValue, context: &mut Context) -> JsResult<blitz_dom::NodeId> {
    let id = this_node_id(this)?;
    let ctx = dom_ctx(context)?;
    let valid = ctx.doc.borrow().get_node(id).is_some_and(|node| {
        node.is_shadow_root() || matches!(node.data, blitz_dom::NodeData::Document(_))
    });
    if !valid {
        return Err(JsNativeError::typ()
            .with_message("Document or ShadowRoot receiver required")
            .into());
    }
    Ok(id)
}

fn sheets_from_array(
    object: &JsObject,
    context: &mut Context,
) -> JsResult<Vec<DocumentStyleSheet>> {
    let length = object
        .get(js_string!("length"), context)?
        .to_length(context)?;
    let mut sheets = Vec::new();
    for index in 0..length {
        sheets.push(sheet_of(&object.get(index as u32, context)?)?);
    }
    Ok(sheets)
}

fn install_array(
    owner: &JsObject,
    values: Vec<JsValue>,
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = owner_id(&owner.clone().into(), context)?;
    let target: JsObject = JsArray::from_iter(values, context).into();
    let sheets = sheets_from_array(&target, context)?;
    define_value(&target, OWNER, owner.clone().into(), context);
    let proxy: JsObject = JsProxyBuilder::new(target.clone())
        .set(adopted_array_set)
        .build(context)?
        .into();
    define_value(owner, ADOPTED_TARGET, target.into(), context);
    define_value(owner, ADOPTED, proxy.clone().into(), context);
    let ctx = dom_ctx(context)?;
    ctx.mutate_doc().set_adopted_stylesheets(id, sheets);
    Ok(proxy.into())
}

fn get_adopted(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    owner_id(this, context)?;
    let owner = this.as_object().unwrap();
    let value = owner.get(JsString::from(ADOPTED), context)?;
    if value.is_undefined() {
        install_array(&owner, Vec::new(), context)
    } else {
        Ok(value)
    }
}

fn set_adopted(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    owner_id(this, context)?;
    let input = args
        .first()
        .and_then(JsValue::as_object)
        .ok_or_else(|| JsNativeError::typ().with_message("adoptedStyleSheets requires an array"))?;
    let length = input
        .get(js_string!("length"), context)?
        .to_length(context)?;
    let mut values = Vec::new();
    for index in 0..length {
        let value = input.get(index as u32, context)?;
        sheet_of(&value)?;
        values.push(value);
    }
    install_array(&this.as_object().unwrap(), values, context)?;
    Ok(JsValue::undefined())
}

fn adopted_array_set(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = args
        .first()
        .and_then(JsValue::as_object)
        .ok_or_else(|| JsNativeError::typ().with_message("Missing adopted stylesheet array"))?;
    let key = args
        .get(1)
        .unwrap_or(&JsValue::undefined())
        .to_property_key(context)?;
    let value = args.get(2).cloned().unwrap_or(JsValue::undefined());
    if args
        .get(1)
        .and_then(JsValue::as_string)
        .is_some_and(|key| key.to_std_string_lossy().parse::<u32>().is_ok())
    {
        sheet_of(&value)?;
    }
    target.set(key, value, true, context)?;
    let owner = target
        .get(JsString::from(OWNER), context)?
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Missing adopted stylesheet owner"))?;
    let active = owner
        .get(JsString::from(ADOPTED_TARGET), context)?
        .as_object();
    if active.is_some_and(|active| JsObject::equals(&active, &target)) {
        let sheets = sheets_from_array(&target, context)?;
        let id = node_id_of_value(&owner.into()).unwrap();
        dom_ctx(context)?
            .mutate_doc()
            .set_adopted_stylesheets(id, sheets);
    }
    Ok(JsValue::from(true))
}
