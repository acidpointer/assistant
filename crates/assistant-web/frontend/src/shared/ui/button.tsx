import type { ButtonHTMLAttributes } from 'react';
/** One focus/disabled contract for toolbar, composer and palette actions. */
export function Button({ className = '', ...props }: ButtonHTMLAttributes<HTMLButtonElement>) {
  return <button type="button" className={`button ${className}`} {...props} />;
}
