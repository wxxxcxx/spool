import AppKit
import QuartzCore

enum BarMotionDirection: Equatable {
  case backward
  case forward

  init(step: Int) {
    self = step < 0 ? .backward : .forward
  }

  var transitionSubtype: CATransitionSubtype {
    self == .forward ? .fromRight : .fromLeft
  }
}

enum BarMotionCurve: Equatable {
  case smooth
  case spring
}

struct BarMotionSpec: Equatable {
  let duration: TimeInterval
  let curve: BarMotionCurve

  static func resolve(
    style: BarAnimationStyle,
    duration: TimeInterval,
    reduceMotion: Bool
  ) -> BarMotionSpec? {
    guard style != .none, !reduceMotion else { return nil }
    return BarMotionSpec(
      duration: duration,
      curve: style == .spring ? .spring : .smooth
    )
  }

  var timingFunction: CAMediaTimingFunction {
    switch curve {
    case .smooth:
      CAMediaTimingFunction(name: .easeInEaseOut)
    case .spring:
      CAMediaTimingFunction(controlPoints: 0.18, 0.89, 0.32, 1.12)
    }
  }
}

enum BarMotion {
  static let workspaceKey = "SpoolBar.workspaceTransition"
  static let windowScrollKey = "SpoolBar.windowScroll"
  static let focusKey = "SpoolBar.focusMove"
  static let focusFadeKey = "SpoolBar.focusFade"
  static let buttonPressKey = "SpoolBar.buttonPress"
  static let buttonHoverKey = "SpoolBar.buttonHover"

  static func addPush(
    to layer: CALayer?,
    direction: BarMotionDirection,
    spec: BarMotionSpec?,
    key: String
  ) {
    guard let layer,
      let transition = pushAnimation(direction: direction, spec: spec)
    else { return }
    layer.add(transition, forKey: key)
  }

  static func pushAnimation(
    direction: BarMotionDirection,
    spec: BarMotionSpec?
  ) -> CATransition? {
    guard let spec else { return nil }
    let transition = CATransition()
    transition.type = .push
    transition.subtype = direction.transitionSubtype
    transition.duration = spec.duration
    transition.timingFunction = spec.timingFunction
    return transition
  }

  static func addFade(to layer: CALayer?, spec: BarMotionSpec?, key: String) {
    guard let layer, let spec else { return }
    let transition = CATransition()
    transition.type = .fade
    transition.duration = min(0.16, spec.duration)
    transition.timingFunction = CAMediaTimingFunction(name: .easeOut)
    layer.add(transition, forKey: key)
  }

  static func addFocusMove(
    to layer: CALayer?,
    from source: CGRect,
    to target: CGRect,
    spec: BarMotionSpec?
  ) {
    guard let layer, let spec, source != target else { return }
    let position = CABasicAnimation(keyPath: "position")
    position.fromValue = CGPoint(x: source.midX, y: source.midY)
    position.toValue = CGPoint(x: target.midX, y: target.midY)

    let bounds = CABasicAnimation(keyPath: "bounds")
    bounds.fromValue = CGRect(origin: .zero, size: source.size)
    bounds.toValue = CGRect(origin: .zero, size: target.size)

    let group = CAAnimationGroup()
    group.animations = [position, bounds]
    group.duration = min(0.2, spec.duration)
    group.timingFunction = spec.timingFunction
    layer.add(group, forKey: focusKey)
  }

  static func setPressed(_ pressed: Bool, on layer: CALayer?, spec: BarMotionSpec?) {
    guard let layer else { return }
    let target: CGFloat = pressed ? 0.91 : 1
    let current = (layer.presentation()?.value(forKeyPath: "transform.scale") as? CGFloat) ?? 1
    layer.setAffineTransform(CGAffineTransform(scaleX: target, y: target))
    guard let spec else { return }
    let animation = CABasicAnimation(keyPath: "transform.scale")
    animation.fromValue = current
    animation.toValue = target
    animation.duration = min(pressed ? 0.08 : 0.14, spec.duration)
    animation.timingFunction = spec.timingFunction
    layer.add(animation, forKey: buttonPressKey)
  }

  static func setHoverColor(
    _ color: CGColor,
    on layer: CALayer,
    spec: BarMotionSpec?
  ) {
    let current = layer.presentation()?.backgroundColor ?? layer.backgroundColor
    layer.backgroundColor = color
    guard let spec else { return }
    let animation = CABasicAnimation(keyPath: "backgroundColor")
    animation.fromValue = current
    animation.toValue = color
    animation.duration = min(0.12, spec.duration)
    animation.timingFunction = CAMediaTimingFunction(name: .easeOut)
    layer.add(animation, forKey: buttonHoverKey)
  }
}
