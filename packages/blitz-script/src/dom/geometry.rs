//! Native Geometry Interfaces objects. Matrices use CSS column-major order.

use std::cell::Cell;

use boa_engine::object::builtins::JsArray;
use boa_engine::object::{FunctionObjectBuilder, JsObject, ObjectInitializer};
use boa_engine::property::{Attribute, PropertyDescriptor};
use boa_engine::{
    Context, Finalize, JsData, JsNativeError, JsResult, JsString, JsSymbol, JsValue,
    NativeFunction, Trace,
};

use super::{define_method, define_value, dom_ctx, js_str, this_node_id};

const IDENTITY: [f64; 16] = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];
const COMPONENTS: [&str; 16] = [
    "m11", "m12", "m13", "m14", "m21", "m22", "m23", "m24", "m31", "m32", "m33", "m34", "m41",
    "m42", "m43", "m44",
];
const ALIASES: [(&str, usize); 6] = [("a", 0), ("b", 1), ("c", 4), ("d", 5), ("e", 12), ("f", 13)];

#[derive(Trace, Finalize, JsData)]
struct Rect {
    #[unsafe_ignore_trace]
    values: Cell<[f64; 4]>,
}

#[derive(Trace, Finalize, JsData)]
struct Point {
    #[unsafe_ignore_trace]
    values: Cell<[f64; 4]>,
}

#[derive(Trace, Finalize, JsData)]
struct Matrix {
    #[unsafe_ignore_trace]
    values: Cell<[f64; 16]>,
    #[unsafe_ignore_trace]
    is_2d: Cell<bool>,
}

fn number(value: Option<&JsValue>, default: f64, context: &mut Context) -> JsResult<f64> {
    match value {
        None => Ok(default),
        Some(value) if value.is_undefined() => Ok(default),
        Some(value) => value.to_number(context),
    }
}

fn dictionary(
    value: Option<&JsValue>,
    names: [&str; 4],
    defaults: [f64; 4],
    context: &mut Context,
) -> JsResult<[f64; 4]> {
    let object = match value {
        None => None,
        Some(value) if value.is_null_or_undefined() => None,
        Some(value) => Some(value.to_object(context)?),
    };
    let mut values = defaults;
    if let Some(object) = object {
        for (index, name) in names.iter().enumerate() {
            let value = object.get(JsString::from(*name), context)?;
            values[index] = number(Some(&value), defaults[index], context)?;
        }
    }
    Ok(values)
}

fn rect(this: &JsValue) -> JsResult<[f64; 4]> {
    this.as_object()
        .and_then(|object| object.downcast_ref::<Rect>().map(|data| data.values.get()))
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("Invalid DOMRect receiver")
                .into()
        })
}

fn point(this: &JsValue) -> JsResult<[f64; 4]> {
    this.as_object()
        .and_then(|object| object.downcast_ref::<Point>().map(|data| data.values.get()))
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("Invalid DOMPoint receiver")
                .into()
        })
}

fn matrix(this: &JsValue) -> JsResult<([f64; 16], bool)> {
    this.as_object()
        .and_then(|object| {
            object
                .downcast_ref::<Matrix>()
                .map(|data| (data.values.get(), data.is_2d.get()))
        })
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("Invalid DOMMatrix receiver")
                .into()
        })
}

fn is_2d(values: &[f64; 16]) -> bool {
    [2, 3, 6, 7, 8, 9, 11, 14]
        .iter()
        .all(|&index| values[index] == 0.0)
        && values[10] == 1.0
        && values[15] == 1.0
}

fn matrix_init(value: Option<&JsValue>, context: &mut Context) -> JsResult<([f64; 16], bool)> {
    let mut values = IDENTITY;
    let object = match value {
        None => return Ok((values, true)),
        Some(value) if value.is_null_or_undefined() => return Ok((values, true)),
        Some(value) => value.to_object(context)?,
    };
    for (index, name) in COMPONENTS.iter().enumerate() {
        let value = object.get(JsString::from(*name), context)?;
        values[index] = number(Some(&value), IDENTITY[index], context)?;
    }
    for (alias, index) in ALIASES {
        let value = object.get(JsString::from(alias), context)?;
        if value.is_undefined() {
            continue;
        }
        let value = value.to_number(context)?;
        let member = object.get(JsString::from(COMPONENTS[index]), context)?;
        if !member.is_undefined()
            && values[index] != value
            && !(values[index].is_nan() && value.is_nan())
        {
            return Err(JsNativeError::typ()
                .with_message("Conflicting DOMMatrix aliases")
                .into());
        }
        values[index] = value;
    }
    let flag = object.get(boa_engine::js_string!("is2D"), context)?;
    let two_d = if flag.is_undefined() {
        is_2d(&values)
    } else {
        flag.to_boolean()
    };
    if two_d && !is_2d(&values) {
        return Err(JsNativeError::typ()
            .with_message("is2D conflicts with matrix components")
            .into());
    }
    Ok((values, two_d))
}

fn matrix_string(value: &str, context: &mut Context) -> JsResult<([f64; 16], bool)> {
    let value = value.trim();
    if value.is_empty() || value == "none" {
        return Ok((IDENTITY, true));
    }
    let (body, count) = if let Some(body) = value
        .strip_prefix("matrix(")
        .and_then(|value| value.strip_suffix(')'))
    {
        (body, 6)
    } else if let Some(body) = value
        .strip_prefix("matrix3d(")
        .and_then(|value| value.strip_suffix(')'))
    {
        (body, 16)
    } else {
        return Err(super::interfaces::exception(
            "SyntaxError",
            "Expected matrix() or matrix3d()",
            context,
        ));
    };
    let numbers = body
        .split(',')
        .map(|part| part.trim().parse::<f64>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| {
            super::interfaces::exception("SyntaxError", "Invalid matrix number", context)
        })?;
    if numbers.len() != count || numbers.iter().any(|value| !value.is_finite()) {
        return Err(super::interfaces::exception(
            "SyntaxError",
            "Invalid matrix",
            context,
        ));
    }
    let mut values = IDENTITY;
    if count == 6 {
        for ((_, index), value) in ALIASES.into_iter().zip(numbers) {
            values[index] = value;
        }
    } else {
        values.copy_from_slice(&numbers);
    }
    Ok((values, count == 6))
}

fn matrix_sequence(value: &JsValue, context: &mut Context) -> JsResult<([f64; 16], bool)> {
    let object = value.to_object(context)?;
    let method = object
        .get(JsSymbol::iterator(), context)?
        .as_object()
        .filter(|method| method.is_callable())
        .ok_or_else(|| JsNativeError::typ().with_message("Matrix sequence must be iterable"))?;
    let iterator = method
        .call(value, &[], context)?
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Invalid matrix iterator"))?;
    let next = iterator
        .get(boa_engine::js_string!("next"), context)?
        .as_object()
        .filter(|method| method.is_callable())
        .ok_or_else(|| JsNativeError::typ().with_message("Invalid matrix iterator"))?;
    let mut numbers = Vec::new();
    loop {
        let item = next
            .call(&iterator.clone().into(), &[], context)?
            .as_object()
            .ok_or_else(|| JsNativeError::typ().with_message("Invalid matrix iterator result"))?;
        if item
            .get(boa_engine::js_string!("done"), context)?
            .to_boolean()
        {
            break;
        }
        numbers.push(
            item.get(boa_engine::js_string!("value"), context)?
                .to_number(context)?,
        );
    }
    let mut values = IDENTITY;
    let is_2d = numbers.len() == 6;
    match numbers.len() {
        6 => {
            for ((_, index), value) in ALIASES.into_iter().zip(numbers) {
                values[index] = value;
            }
        }
        16 => values.copy_from_slice(&numbers),
        _ => {
            return Err(JsNativeError::typ()
                .with_message("Matrix requires 6 or 16 values")
                .into());
        }
    }
    Ok((values, is_2d))
}

fn new_rect(name: &str, values: [f64; 4], context: &Context) -> JsObject {
    JsObject::from_proto_and_data(
        Some(super::interfaces::prototype(name, context)),
        Rect {
            values: Cell::new(values),
        },
    )
}

fn new_point(name: &str, values: [f64; 4], context: &Context) -> JsObject {
    JsObject::from_proto_and_data(
        Some(super::interfaces::prototype(name, context)),
        Point {
            values: Cell::new(values),
        },
    )
}

fn new_matrix(name: &str, values: [f64; 16], two_d: bool, context: &Context) -> JsObject {
    JsObject::from_proto_and_data(
        Some(super::interfaces::prototype(name, context)),
        Matrix {
            values: Cell::new(values),
            is_2d: Cell::new(two_d),
        },
    )
}

fn multiply(a: [f64; 16], b: [f64; 16]) -> [f64; 16] {
    let mut result = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            result[column * 4 + row] = (0..4)
                .map(|index| a[index * 4 + row] * b[column * 4 + index])
                .sum();
        }
    }
    result
}

fn inverse(values: [f64; 16]) -> [f64; 16] {
    let mut rows = [[0.0; 8]; 4];
    for row in 0..4 {
        for column in 0..4 {
            rows[row][column] = values[column * 4 + row];
        }
        rows[row][row + 4] = 1.0;
    }
    for column in 0..4 {
        let mut pivot = column;
        for row in column + 1..4 {
            if rows[row][column].abs() > rows[pivot][column].abs() {
                pivot = row;
            }
        }
        if rows[pivot][column] == 0.0 || !rows[pivot][column].is_finite() {
            return [f64::NAN; 16];
        }
        rows.swap(column, pivot);
        let divisor = rows[column][column];
        for value in &mut rows[column] {
            *value /= divisor;
        }
        for row in 0..4 {
            if row == column {
                continue;
            }
            let factor = rows[row][column];
            for index in 0..8 {
                rows[row][index] -= factor * rows[column][index];
            }
        }
    }
    let mut result = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            result[column * 4 + row] = rows[row][column + 4];
        }
    }
    result
}

fn translation(x: f64, y: f64, z: f64) -> [f64; 16] {
    let mut values = IDENTITY;
    values[12] = x;
    values[13] = y;
    values[14] = z;
    values
}

fn rotation(x: f64, y: f64, z: f64) -> [f64; 16] {
    let (sx, cx) = x.to_radians().sin_cos();
    let (sy, cy) = y.to_radians().sin_cos();
    let (sz, cz) = z.to_radians().sin_cos();
    let rx = [
        1.0, 0.0, 0.0, 0.0, 0.0, cx, sx, 0.0, 0.0, -sx, cx, 0.0, 0.0, 0.0, 0.0, 1.0,
    ];
    let ry = [
        cy, 0.0, -sy, 0.0, 0.0, 1.0, 0.0, 0.0, sy, 0.0, cy, 0.0, 0.0, 0.0, 0.0, 1.0,
    ];
    let rz = [
        cz, sz, 0.0, 0.0, -sz, cz, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ];
    multiply(multiply(rz, ry), rx)
}

fn transformed(values: [f64; 16], point: [f64; 4]) -> [f64; 4] {
    std::array::from_fn(|row| {
        (0..4)
            .map(|column| values[column * 4 + row] * point[column])
            .sum()
    })
}

fn matrix_operation(
    operation: u8,
    mutate: bool,
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let (values, mut two_d) = matrix(this)?;
    let next = match operation {
        0 | 1 => {
            let (other, other_2d) = matrix_init(args.first(), context)?;
            two_d &= other_2d;
            if operation == 0 {
                multiply(values, other)
            } else {
                multiply(other, values)
            }
        }
        2 => {
            let result = inverse(values);
            if result.iter().any(|value| value.is_nan()) {
                two_d = false;
            }
            result
        }
        3 => {
            let x = number(args.first(), 0.0, context)?;
            let y = number(args.get(1), 0.0, context)?;
            let z = number(args.get(2), 0.0, context)?;
            two_d &= z == 0.0;
            multiply(values, translation(x, y, z))
        }
        4 => {
            let x = number(args.first(), 1.0, context)?;
            let y = number(args.get(1), x, context)?;
            let z = number(args.get(2), 1.0, context)?;
            let ox = number(args.get(3), 0.0, context)?;
            let oy = number(args.get(4), 0.0, context)?;
            let oz = number(args.get(5), 0.0, context)?;
            two_d &= z == 1.0 && oz == 0.0;
            let mut scale = IDENTITY;
            scale[0] = x;
            scale[5] = y;
            scale[10] = z;
            multiply(
                multiply(multiply(values, translation(ox, oy, oz)), scale),
                translation(-ox, -oy, -oz),
            )
        }
        5 => {
            let mut x = number(args.first(), 0.0, context)?;
            let mut y = number(args.get(1), 0.0, context)?;
            let mut z = number(args.get(2), 0.0, context)?;
            if args.get(1).is_none_or(JsValue::is_undefined)
                && args.get(2).is_none_or(JsValue::is_undefined)
            {
                z = x;
                x = 0.0;
                y = 0.0;
            }
            two_d &= x == 0.0 && y == 0.0;
            multiply(values, rotation(x, y, z))
        }
        6 | 7 => {
            let mut flip = IDENTITY;
            flip[if operation == 6 { 0 } else { 5 }] = -1.0;
            multiply(values, flip)
        }
        8 | 9 => {
            let angle = number(args.first(), 0.0, context)?.to_radians().tan();
            let mut skew = IDENTITY;
            skew[if operation == 8 { 4 } else { 1 }] = angle;
            multiply(values, skew)
        }
        _ => unreachable!(),
    };
    if mutate {
        let object = this.as_object().expect("validated matrix");
        let data = object.downcast_ref::<Matrix>().expect("validated matrix");
        data.values.set(next);
        data.is_2d.set(two_d);
        Ok(this.clone())
    } else {
        Ok(new_matrix("DOMMatrix", next, two_d, context).into())
    }
}

fn method(
    proto: &JsObject,
    name: &'static str,
    length: usize,
    body: NativeFunction,
    context: &mut Context,
) {
    let function = FunctionObjectBuilder::new(context.realm(), body)
        .name(JsString::from(name))
        .length(length)
        .build();
    define_value(proto, name, function.into(), context);
}

fn component(
    proto: &JsObject,
    name: &str,
    index: usize,
    kind: u8,
    writable: bool,
    context: &mut Context,
) {
    let getter = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure(move |this, _, _| {
            let value = match kind {
                0 => rect(this)?[index],
                1 => point(this)?[index],
                2 => matrix(this)?.0[index],
                3 => {
                    let [x, y, width, height] = rect(this)?;
                    match index {
                        0 => y.min(y + height),
                        1 => (x + width).max(x),
                        2 => (y + height).max(y),
                        3 => x.min(x + width),
                        _ => unreachable!(),
                    }
                }
                _ => unreachable!(),
            };
            Ok(JsValue::from(value))
        }),
    )
    .name(JsString::from(format!("get {name}")))
    .length(0)
    .build();
    let mut descriptor = PropertyDescriptor::builder()
        .get(getter)
        .enumerable(true)
        .configurable(true);
    if writable {
        let setter = FunctionObjectBuilder::new(
            context.realm(),
            NativeFunction::from_copy_closure(move |this, args, context| {
                let value = number(args.first(), f64::NAN, context)?;
                let object = this.as_object().ok_or_else(|| {
                    JsNativeError::typ().with_message("Invalid geometry receiver")
                })?;
                match kind {
                    0 => {
                        let data = object.downcast_ref::<Rect>().ok_or_else(|| {
                            JsNativeError::typ().with_message("Invalid DOMRect receiver")
                        })?;
                        let mut values = data.values.get();
                        values[index] = value;
                        data.values.set(values);
                    }
                    1 => {
                        let data = object.downcast_ref::<Point>().ok_or_else(|| {
                            JsNativeError::typ().with_message("Invalid DOMPoint receiver")
                        })?;
                        let mut values = data.values.get();
                        values[index] = value;
                        data.values.set(values);
                    }
                    2 => {
                        let data = object.downcast_ref::<Matrix>().ok_or_else(|| {
                            JsNativeError::typ().with_message("Invalid DOMMatrix receiver")
                        })?;
                        let mut values = data.values.get();
                        values[index] = value;
                        data.values.set(values);
                        if !is_2d(&values) {
                            data.is_2d.set(false);
                        }
                    }
                    _ => unreachable!(),
                }
                Ok(JsValue::undefined())
            }),
        )
        .name(JsString::from(format!("set {name}")))
        .length(1)
        .build();
        descriptor = descriptor.set(setter);
    }
    proto
        .define_property_or_throw(JsString::from(name), descriptor, context)
        .expect("failed to define geometry component");
}

fn json(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let Ok([x, y, width, height]) = rect(this) {
        return Ok(ObjectInitializer::new(context)
            .property(boa_engine::js_string!("x"), x, Attribute::all())
            .property(boa_engine::js_string!("y"), y, Attribute::all())
            .property(boa_engine::js_string!("width"), width, Attribute::all())
            .property(boa_engine::js_string!("height"), height, Attribute::all())
            .property(
                boa_engine::js_string!("top"),
                y.min(y + height),
                Attribute::all(),
            )
            .property(
                boa_engine::js_string!("right"),
                x.max(x + width),
                Attribute::all(),
            )
            .property(
                boa_engine::js_string!("bottom"),
                y.max(y + height),
                Attribute::all(),
            )
            .property(
                boa_engine::js_string!("left"),
                x.min(x + width),
                Attribute::all(),
            )
            .build()
            .into());
    }
    if let Ok([x, y, z, w]) = point(this) {
        return Ok(ObjectInitializer::new(context)
            .property(boa_engine::js_string!("x"), x, Attribute::all())
            .property(boa_engine::js_string!("y"), y, Attribute::all())
            .property(boa_engine::js_string!("z"), z, Attribute::all())
            .property(boa_engine::js_string!("w"), w, Attribute::all())
            .build()
            .into());
    }
    let (values, two_d) = matrix(this)?;
    let object = JsObject::with_object_proto(context.intrinsics());
    for (name, index) in COMPONENTS
        .iter()
        .enumerate()
        .map(|(index, name)| (*name, index))
        .chain(ALIASES)
    {
        object.define_property_or_throw(
            JsString::from(name),
            PropertyDescriptor::builder()
                .value(values[index])
                .writable(true)
                .enumerable(true)
                .configurable(true),
            context,
        )?;
    }
    for (name, value) in [("is2D", two_d), ("isIdentity", values == IDENTITY)] {
        object.define_property_or_throw(
            JsString::from(name),
            PropertyDescriptor::builder()
                .value(value)
                .writable(true)
                .enumerable(true)
                .configurable(true),
            context,
        )?;
    }
    Ok(object.into())
}

fn transform_point(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let values = matrix(this)?.0;
    let point = dictionary(
        args.first(),
        ["x", "y", "z", "w"],
        [0.0, 0.0, 0.0, 1.0],
        context,
    )?;
    Ok(new_point("DOMPoint", transformed(values, point), context).into())
}

fn point_transform(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let point = point(this)?;
    let values = matrix_init(args.first(), context)?.0;
    Ok(new_point("DOMPoint", transformed(values, point), context).into())
}

fn matrix_to_string(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (values, two_d) = matrix(this)?;
    if values.iter().any(|value| !value.is_finite()) {
        return Err(super::interfaces::exception(
            "InvalidStateError",
            "Cannot serialise a non-finite matrix",
            context,
        ));
    }
    let fields: Vec<String> = if two_d {
        ALIASES
            .iter()
            .map(|(_, index)| values[*index].to_string())
            .collect()
    } else {
        values.iter().map(ToString::to_string).collect()
    };
    Ok(js_str(&format!(
        "{}({})",
        if two_d { "matrix" } else { "matrix3d" },
        fields.join(", ")
    )))
}

fn boxes(this: &JsValue, context: &mut Context) -> JsResult<Vec<[f64; 4]>> {
    let id = this_node_id(this)?;
    let ctx = dom_ctx(context)?;
    ctx.flush_layout();
    let doc = ctx.doc.borrow();
    let Some(node) = doc.get_node(id) else {
        return Ok(Vec::new());
    };
    if !node.flags.is_in_document()
        || node
            .primary_styles()
            .is_none_or(|style| style.clone_display().is_contents())
    {
        return Ok(Vec::new());
    }
    let mut current = Some(id);
    while let Some(id) = current {
        let Some(node) = doc.get_node(id) else {
            return Ok(Vec::new());
        };
        if node.is_element() && node.is_display_none() {
            return Ok(Vec::new());
        }
        current = node.parent;
    }
    Ok(doc
        .node_client_rects(id)
        .into_iter()
        .map(|rect| [rect.x, rect.y, rect.width, rect.height])
        .collect())
}

fn bounding_rect(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let boxes = boxes(this, context)?;
    let mut nonempty = boxes.iter().filter(|rect| rect[2] != 0.0 || rect[3] != 0.0);
    let values = if let Some(first) = nonempty.next() {
        let mut left = first[0];
        let mut top = first[1];
        let mut right = first[0] + first[2];
        let mut bottom = first[1] + first[3];
        for rect in nonempty {
            left = left.min(rect[0]);
            top = top.min(rect[1]);
            right = right.max(rect[0] + rect[2]);
            bottom = bottom.max(rect[1] + rect[3]);
        }
        [left, top, right - left, bottom - top]
    } else {
        boxes.first().copied().unwrap_or([0.0; 4])
    };
    Ok(new_rect("DOMRect", values, context).into())
}

fn client_rects(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let boxes = boxes(this, context)?;
    let items = boxes
        .into_iter()
        .map(|values| new_rect("DOMRect", values, context).into())
        .collect();
    super::collections::snapshot("DOMRectList", items, context)
}

pub(super) fn init(element: &JsObject, context: &mut Context) {
    for (name, parent, kind, writable) in [
        ("DOMRectReadOnly", None, 0, false),
        ("DOMRect", Some("DOMRectReadOnly"), 0, true),
        ("DOMPointReadOnly", None, 1, false),
        ("DOMPoint", Some("DOMPointReadOnly"), 1, true),
        ("DOMMatrixReadOnly", None, 2, false),
        ("DOMMatrix", Some("DOMMatrixReadOnly"), 2, true),
    ] {
        let proto = JsObject::with_object_proto(context.intrinsics());
        super::interfaces::register(
            name,
            parent,
            proto.clone(),
            if kind == 2 { 0 } else { 0 },
            NativeFunction::from_copy_closure(move |target, args, context| {
                let proto = super::interfaces::construction_prototype(target, name, context)?;
                let object = match kind {
                    0 | 1 => {
                        let mut values = [0.0; 4];
                        if kind == 1 {
                            values[3] = 1.0;
                        }
                        for (index, value) in values.iter_mut().enumerate() {
                            *value = number(args.get(index), *value, context)?;
                        }
                        if kind == 0 {
                            new_rect(name, values, context)
                        } else {
                            new_point(name, values, context)
                        }
                    }
                    2 => {
                        let (values, two_d) = match args.first() {
                            None => (IDENTITY, true),
                            Some(value) if value.is_undefined() => (IDENTITY, true),
                            Some(value) if value.is_string() => {
                                matrix_string(&super::to_rust_string(value, context)?, context)?
                            }
                            Some(value) => matrix_sequence(value, context)?,
                        };
                        new_matrix(name, values, two_d, context)
                    }
                    _ => unreachable!(),
                };
                object.set_prototype(Some(proto));
                Ok(object.into())
            }),
            context,
        );
        if kind != 2 {
            let names = if kind == 0 {
                ["x", "y", "width", "height"]
            } else {
                ["x", "y", "z", "w"]
            };
            for (index, field) in names.iter().enumerate() {
                component(&proto, field, index, kind, writable, context);
            }
            if kind == 0 {
                for (index, field) in ["top", "right", "bottom", "left"].iter().enumerate() {
                    component(&proto, field, index, 3, false, context);
                }
            } else {
                define_method(&proto, "matrixTransform", 0, point_transform, context);
            }
            let constructor = super::interfaces::constructor(name, context);
            method(
                &constructor,
                if kind == 0 { "fromRect" } else { "fromPoint" },
                0,
                NativeFunction::from_copy_closure(move |_, args, context| {
                    let values = if kind == 0 {
                        dictionary(
                            args.first(),
                            ["x", "y", "width", "height"],
                            [0.0; 4],
                            context,
                        )?
                    } else {
                        dictionary(
                            args.first(),
                            ["x", "y", "z", "w"],
                            [0.0, 0.0, 0.0, 1.0],
                            context,
                        )?
                    };
                    Ok(if kind == 0 {
                        new_rect(name, values, context)
                    } else {
                        new_point(name, values, context)
                    }
                    .into())
                }),
                context,
            );
        } else {
            for (index, field) in COMPONENTS.iter().enumerate() {
                component(&proto, field, index, 2, writable, context);
            }
            for (field, index) in ALIASES {
                component(&proto, field, index, 2, writable, context);
            }
            for (field, identity) in [("is2D", false), ("isIdentity", true)] {
                let getter = FunctionObjectBuilder::new(
                    context.realm(),
                    NativeFunction::from_copy_closure(move |this, _, _| {
                        let (values, two_d) = matrix(this)?;
                        Ok(JsValue::from(if identity {
                            values == IDENTITY
                        } else {
                            two_d
                        }))
                    }),
                )
                .name(JsString::from(format!("get {field}")))
                .length(0)
                .build();
                proto
                    .define_property_or_throw(
                        JsString::from(field),
                        PropertyDescriptor::builder()
                            .get(getter)
                            .enumerable(true)
                            .configurable(true),
                        context,
                    )
                    .expect("failed to define matrix flag");
            }
            for (field, operation, length) in [
                ("multiply", 0, 0),
                ("inverse", 2, 0),
                ("translate", 3, 0),
                ("scale", 4, 0),
                ("rotate", 5, 0),
                ("flipX", 6, 0),
                ("flipY", 7, 0),
                ("skewX", 8, 0),
                ("skewY", 9, 0),
            ] {
                method(
                    &proto,
                    field,
                    length,
                    NativeFunction::from_copy_closure(move |this, args, context| {
                        matrix_operation(operation, false, this, args, context)
                    }),
                    context,
                );
            }
            if writable {
                for (field, operation) in [
                    ("multiplySelf", 0),
                    ("preMultiplySelf", 1),
                    ("invertSelf", 2),
                    ("translateSelf", 3),
                    ("scaleSelf", 4),
                    ("rotateSelf", 5),
                    ("skewXSelf", 8),
                    ("skewYSelf", 9),
                ] {
                    method(
                        &proto,
                        field,
                        0,
                        NativeFunction::from_copy_closure(move |this, args, context| {
                            matrix_operation(operation, true, this, args, context)
                        }),
                        context,
                    );
                }
                method(
                    &proto,
                    "setMatrixValue",
                    1,
                    NativeFunction::from_copy_closure(|this, args, context| {
                        matrix(this)?;
                        let text = super::to_rust_string(
                            args.first().unwrap_or(&JsValue::undefined()),
                            context,
                        )?;
                        let (values, two_d) = matrix_string(&text, context)?;
                        let object = this.as_object().expect("validated matrix");
                        let data = object.downcast_ref::<Matrix>().expect("validated matrix");
                        data.values.set(values);
                        data.is_2d.set(two_d);
                        Ok(this.clone())
                    }),
                    context,
                );
            }
            define_method(&proto, "transformPoint", 0, transform_point, context);
            define_method(&proto, "toString", 0, matrix_to_string, context);
            for field in ["toFloat32Array", "toFloat64Array"] {
                method(
                    &proto,
                    field,
                    0,
                    NativeFunction::from_copy_closure(move |this, _, context| {
                        let values = matrix(this)?.0;
                        let array =
                            JsArray::from_iter(values.into_iter().map(JsValue::from), context);
                        let name = if field == "toFloat32Array" {
                            "Float32Array"
                        } else {
                            "Float64Array"
                        };
                        let constructor = context
                            .global_object()
                            .get(JsString::from(name), context)?
                            .as_object()
                            .expect("missing typed array constructor");
                        Ok(constructor
                            .construct(&[array.into()], None, context)?
                            .into())
                    }),
                    context,
                );
            }
            let constructor = super::interfaces::constructor(name, context);
            method(
                &constructor,
                "fromMatrix",
                0,
                NativeFunction::from_copy_closure(move |_, args, context| {
                    let (values, two_d) = matrix_init(args.first(), context)?;
                    Ok(new_matrix(name, values, two_d, context).into())
                }),
                context,
            );
            for (field, array_name) in [
                ("fromFloat32Array", "Float32Array"),
                ("fromFloat64Array", "Float64Array"),
            ] {
                method(
                    &constructor,
                    field,
                    1,
                    NativeFunction::from_copy_closure(move |_, args, context| {
                        let value = &args.first().cloned().unwrap_or_default();
                        let constructor = context
                            .global_object()
                            .get(JsString::from(array_name), context)?;
                        if !value.instance_of(&constructor, context)? {
                            return Err(JsNativeError::typ()
                                .with_message("Wrong typed array type")
                                .into());
                        }
                        let (values, two_d) = matrix_sequence(value, context)?;
                        Ok(new_matrix(name, values, two_d, context).into())
                    }),
                    context,
                );
            }
        }
        define_method(&proto, "toJSON", 0, json, context);
    }
    define_method(element, "getBoundingClientRect", 0, bounding_rect, context);
    define_method(element, "getClientRects", 0, client_rects, context);
}
